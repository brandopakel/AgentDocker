//! A project's memory: documents of their own kind, by project, kept on
//! this machine. Any agent of the project or the person adds one and
//! retires one; its author or the person forgets it. A retirement is a
//! document of a kind of its own, so a daemon that knows nothing of
//! retirements reads none and says the retired memory again, never a
//! reason as if it were a fact.
use super::*;
use agentdocker_core::memory::{self, MEMORIES_PER_PROJECT, Memory, Reader};

/// A memory just kept, and the ones it may be compared with, by id and text.
type Kept = (Memory, Vec<(String, String)>);

pub(in crate::daemon) const MEMORY: &str = "memory";
/// The word that a memory is no longer true ([`Memory::retiring`]).
pub(in crate::daemon) const RETIREMENT: &str = "memory_retirement";

/// The kind a memory is stored under: its own, or a retirement's.
fn kind_of(memory: &Memory) -> &'static str {
    if memory.retirement {
        RETIREMENT
    } else {
        MEMORY
    }
}

fn memory_doc(memory: &Memory) -> Doc {
    doc(kind_of(memory), memory.id.clone(), memory)
}

/// The journal's line for a retirement. The journal is read by everyone
/// in the project and told to new sessions, with no reader's roles
/// applied, so it says only what everyone may read: for a memory said to
/// everyone, its first words and the reason; for one said to some roles,
/// its id alone, with which roles can read the rest in its history.
fn retired_summary(retired: &Memory, retirement: &Memory) -> String {
    if retired.roles.is_empty() {
        format!(
            "retired memory {} (\"{}\"): {}",
            retired.id,
            memory::headline(&retired.text),
            retirement.text
        )
    } else {
        format!(
            "retired memory {}, one said to {} only: its text and the reason are in its history, for those who can read it",
            retired.id,
            retired.roles.join(", ")
        )
    }
}

/// Who reads a project's memory, by a request's `from`: the person, or an
/// agent with the roles it holds. A request that names nobody reads what
/// is for everyone, so a memory said to some roles never reaches a client
/// that did not say who it is (the `from` it names is checked against its
/// sender before this).
enum Reading {
    Person,
    Holding(Vec<String>),
}

impl Reading {
    fn reader(&self) -> Reader<'_> {
        match self {
            Reading::Person => Reader::Person,
            Reading::Holding(roles) => Reader::Holding(roles),
        }
    }
}

/// A memory search as the request spells it: who asks and for which
/// project, the words, how many to return, the moment to read as of,
/// whether superseded memories count, and the score below which none do.
pub(in crate::daemon) struct MemorySearchArgs {
    pub from: Option<String>,
    pub project: Option<String>,
    pub query: String,
    pub limit: Option<usize>,
    pub as_of: Option<chrono::DateTime<Utc>>,
    pub include_superseded: bool,
    pub min_score: Option<f64>,
}

impl State {
    /// How `from` reads `project`'s memory. Roles are a project's own, so
    /// an agent of another project holds none of them here and reads what
    /// is for everyone.
    fn reading(
        &mut self,
        from: Option<&str>,
        project: &ProjectId,
    ) -> Result<Reading, Box<Response>> {
        let Some(from) = from else {
            return Ok(Reading::Holding(Vec::new()));
        };
        let actor = self.actor(from)?;
        if actor.human {
            return Ok(Reading::Person);
        }
        if actor.project.as_ref() != Some(project) {
            return Ok(Reading::Holding(Vec::new()));
        }
        let roles = self
            .registry
            .resolve(&actor.id)
            .ok()
            .and_then(|id| self.registry.get(&id))
            .map(|record| record.roles().into_iter().map(str::to_owned).collect())
            .unwrap_or_default();
        Ok(Reading::Holding(roles))
    }

    /// Every memory of a project, superseded ones included, and the
    /// retirements beside them: core reads them as one list.
    pub(in crate::daemon) fn project_memories(
        &mut self,
        project: &ProjectId,
    ) -> Result<Vec<Memory>, Box<Response>> {
        self.store_read("memories", |store| {
            let mut all = store.documents_where::<Memory>(
                MEMORY,
                "project",
                project.as_str(),
                MEMORIES_PER_PROJECT,
            )?;
            all.extend(store.documents_where::<Memory>(
                RETIREMENT,
                "project",
                project.as_str(),
                MEMORIES_PER_PROJECT,
            )?);
            Ok(all)
        })
        .ok_or_else(|| {
            Box::new(self.storage_failure().unwrap_or_else(|| {
                Response::error(
                    ErrorCode::StorageUnavailable,
                    "the project's memory could not be read",
                )
            }))
        })
    }

    /// A memory by id or unique id prefix, within the project when one is
    /// known.
    fn resolve_memory(
        &mut self,
        words: &str,
        project: Option<&ProjectId>,
    ) -> Result<Memory, Box<Response>> {
        let words = words.trim();
        for kind in [MEMORY, RETIREMENT] {
            if let Some(Some(found)) =
                self.store_read("memory", |store| store.document::<Memory>(kind, words))
            {
                return Ok(found);
            }
        }
        let Some(project) = project else {
            return Err(Box::new(Response::not_found(format!(
                "no memory is {words}"
            ))));
        };
        let memories = self.project_memories(project)?;
        let matches: Vec<&Memory> = memories
            .iter()
            .filter(|m| words.len() >= 4 && m.id.starts_with(words))
            .collect();
        match matches.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(Box::new(Response::not_found(format!(
                "no memory is {words}"
            )))),
            _ => Err(Box::new(Response::error(
                ErrorCode::Ambiguous,
                format!("more than one memory starts {words}; give more of its id"),
            ))),
        }
    }
}

impl Daemon {
    /// The project a memory request names, else the caller's own.
    async fn memory_project(
        self: &Arc<Self>,
        from: Option<&str>,
        project: Option<String>,
    ) -> Result<ProjectId, Box<Response>> {
        if let Some(selector) = project {
            return self.resolve_project(&selector).await;
        }
        let actor = match from {
            Some(from) => lock(&self.state).actor(from)?.project,
            None => None,
        };
        actor.ok_or_else(|| Box::new(Response::error(ErrorCode::Invalid, "name a project")))
    }

    // Kept as the request spells it while the memory work in flight changes this signature.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::daemon) async fn memory_add(
        self: &Arc<Self>,
        from: &str,
        project: Option<String>,
        text: &str,
        links: Vec<agentdocker_core::Link>,
        supersedes: Option<String>,
        standing: bool,
        forget_after: Option<chrono::DateTime<Utc>>,
        roles: Vec<String>,
        valid_from: Option<chrono::DateTime<Utc>>,
    ) -> Response {
        let project = match self.memory_project(Some(from), project).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        let (memory, candidates) = match self.keep_memory(
            from,
            project,
            text,
            links,
            supersedes,
            standing,
            forget_after,
            roles,
            valid_from,
        ) {
            Ok(kept) => kept,
            Err(response) => return *response,
        };
        // It is kept, and that is the answer: asking the judge changes
        // nothing, whatever it answers, so nothing here holds a transfer
        // back while it does, and nothing it writes (a failure announced)
        // can turn the kept memory into a refusal its client would retry.
        self.answer_settled();
        // A standing memory with no room in what a new session is told is
        // kept all the same; its adder is told, since nobody else will be.
        let left_out = if memory.standing {
            self.no_room(from, &memory)
        } else {
            None
        };
        let (suggestion, may_supersede) = self.judge_memory(&memory.text, candidates).await;
        Response::Memory {
            memory,
            may_supersede,
            suggestion: Some(suggestion),
            left_out,
        }
    }

    /// Whether a memory just kept has no room in what its adder would be
    /// told at the start of a session. A read that fails says nothing: the
    /// memory is kept either way.
    fn no_room(self: &Arc<Self>, from: &str, kept: &memory::Memory) -> Option<memory::NoRoom> {
        let mut state = lock(&self.state);
        let reading = state.reading(Some(from), &kept.project).ok()?;
        let memories = state.project_memories(&kept.project).ok()?;
        memory::no_room(&memories, &kept.id, reading.reader(), Utc::now())
    }

    /// Keep a memory under the state lock, with the current memories it may
    /// be compared with, by id and text ([`memory::comparable`]).
    #[allow(clippy::too_many_arguments)]
    fn keep_memory(
        &self,
        from: &str,
        project: ProjectId,
        text: &str,
        links: Vec<agentdocker_core::Link>,
        supersedes: Option<String>,
        standing: bool,
        forget_after: Option<chrono::DateTime<Utc>>,
        roles: Vec<String>,
        valid_from: Option<chrono::DateTime<Utc>>,
    ) -> Result<Kept, Box<Response>> {
        let refuse = |response: Response| Err(Box::new(response));
        let mut state = lock(&self.state);
        let actor = state.actor(from)?;
        if let Some(error) = state.write_failure() {
            return refuse(error);
        }
        let existing = state.project_memories(&project)?;
        if existing.iter().filter(|m| !m.retirement).count() >= MEMORIES_PER_PROJECT {
            return refuse(Response::error(
                ErrorCode::Invalid,
                "a project keeps at most 1000 memories; forget some first",
            ));
        }
        // The roles it is said to are the project's own.
        if !roles.is_empty() {
            let catalog = state.project_roles(&project)?;
            let unknown: Vec<&str> = roles
                .iter()
                .filter(|r| !catalog.iter().any(|c| &c.role == *r))
                .map(String::as_str)
                .collect();
            if !unknown.is_empty() {
                let names: Vec<&str> = catalog.iter().map(|r| r.role.as_str()).collect();
                return refuse(Response::Error {
                    code: ErrorCode::Invalid,
                    message: format!(
                        "the project has no role called {}; its roles are {}",
                        unknown.join(", "),
                        names.join(", ")
                    ),
                    details: Some(serde_json::json!({ "roles": names })),
                });
            }
        }
        let reading = state.reading(Some(from), &project)?;
        // What it replaces is one of this project's, by its full id, and
        // one its author may read.
        let supersedes = match supersedes {
            Some(words) => match state.resolve_memory(&words, Some(&project)) {
                Ok(old) if old.project == project && !old.seen_by(reading.reader()) => {
                    return refuse(Response::error(
                        ErrorCode::NotFound,
                        format!("no memory is {words}"),
                    ));
                }
                Ok(old) if old.project == project && old.retirement => {
                    return refuse(Response::error(
                        ErrorCode::Invalid,
                        "that is a retirement, not a memory: forget it to bring back the memory it retired",
                    ));
                }
                Ok(old) if old.project == project => Some(old.id),
                Ok(_) => {
                    return refuse(Response::error(
                        ErrorCode::Invalid,
                        "a memory replaces one of its own project's",
                    ));
                }
                Err(response) => return Err(response),
            },
            None => None,
        };
        let memory = match Memory::new(
            project.clone(),
            &actor.id,
            text,
            links,
            supersedes,
            Utc::now(),
        )
        .and_then(|memory| memory.standing(standing).forget_after(forget_after))
        .and_then(|memory| memory.roles(roles))
        .and_then(|memory| memory.valid_from(valid_from))
        {
            Ok(memory) => memory,
            Err(reason) => return refuse(Response::error(ErrorCode::Invalid, reason)),
        };
        let candidates = memory::comparable(
            &existing,
            &memory,
            agentdocker_core::judgment::MEMORY_CANDIDATES,
            reading.reader(),
            Utc::now(),
        )
        .into_iter()
        .map(|m| (m.id.clone(), m.text.clone()))
        .collect();
        let kinds = vec![EventKind::MemoryAdded {
            project,
            memory: memory.id.clone(),
            by: actor.id,
        }];
        if let Some(error) = state.commit_documents("memory", vec![memory_doc(&memory)], kinds) {
            return refuse(error);
        }
        Ok((memory, candidates))
    }

    pub(in crate::daemon) async fn memory_search(
        self: &Arc<Self>,
        args: MemorySearchArgs,
    ) -> Response {
        let MemorySearchArgs {
            from,
            project,
            query,
            limit,
            as_of,
            include_superseded,
            min_score,
        } = args;
        let query = query.as_str();
        let project = match self.memory_project(from.as_deref(), project).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        let (memories, reading) = {
            let mut state = lock(&self.state);
            let reading = match state.reading(from.as_deref(), &project) {
                Ok(reading) => reading,
                Err(response) => return *response,
            };
            match state.project_memories(&project) {
                Ok(memories) => (memories, reading),
                Err(response) => return *response,
            }
        };
        // Read as they stood then, or now; a moment to come is now.
        let now = Utc::now();
        let at = as_of.map_or(now, |at| at.min(now));
        let limit = limit.unwrap_or(20).clamp(1, 100);
        if include_superseded {
            let (found, replaced) = memory::search_history(
                &memories,
                query,
                limit,
                min_score.unwrap_or(0.0),
                reading.reader(),
                at,
            );
            return Response::MemoryHistory {
                memories: found.into_iter().cloned().collect(),
                replaced,
            };
        }
        Response::Memories {
            memories: memory::search_above(
                &memories,
                query,
                limit,
                min_score.unwrap_or(0.0),
                reading.reader(),
                at,
            )
            .into_iter()
            .cloned()
            .collect(),
            left_out: Vec::new(),
        }
    }

    pub(in crate::daemon) async fn memory_profile(
        self: &Arc<Self>,
        from: Option<String>,
        project: Option<String>,
        max_chars: Option<usize>,
        as_of: Option<chrono::DateTime<Utc>>,
    ) -> Response {
        let project = match self.memory_project(from.as_deref(), project).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        let (memories, reading) = {
            let mut state = lock(&self.state);
            let reading = match state.reading(from.as_deref(), &project) {
                Ok(reading) => reading,
                Err(response) => return *response,
            };
            match state.project_memories(&project) {
                Ok(memories) => (memories, reading),
                Err(response) => return *response,
            }
        };
        let told = memory::told(
            &memories,
            max_chars.unwrap_or(4000).clamp(1, 64_000),
            reading.reader(),
            as_of.map_or_else(Utc::now, |at| at.min(Utc::now())),
        );
        Response::Memories {
            left_out: told
                .left_out
                .iter()
                .map(|m| memory::LeftOut::of(m))
                .collect(),
            memories: told.memories.into_iter().cloned().collect(),
        }
    }

    pub(in crate::daemon) fn memory_forget(self: &Arc<Self>, from: &str, id: &str) -> Response {
        let mut state = lock(&self.state);
        let actor = match state.actor(from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if let Some(error) = state.write_failure() {
            return error;
        }
        let memory = match state.resolve_memory(id, actor.project.as_ref()) {
            Ok(memory) => memory,
            Err(response) => return *response,
        };
        // An agent forgets only what it kept, and only in its own project,
        // whether it names the memory by its full id or a prefix. A
        // retirement is also undone by whoever kept the memory it retired.
        let own_project = actor.project.as_ref() == Some(&memory.project);
        let retired_own = memory.retirement
            && memory.supersedes.as_deref().is_some_and(|retired| {
                state
                    .store_read("memory", |store| store.document::<Memory>(MEMORY, retired))
                    .flatten()
                    .is_some_and(|retired| retired.author == actor.id)
            });
        if !actor.human && !(own_project && (memory.author == actor.id || retired_own)) {
            return Response::error(
                ErrorCode::Forbidden,
                "a memory is forgotten by whoever kept it, in its own project, or by the person",
            );
        }
        // What retired it goes with it: a retirement of nothing would
        // stay in the store for good.
        let mut delete = vec![(kind_of(&memory), memory.id.clone())];
        if !memory.retirement {
            let all = match state.project_memories(&memory.project) {
                Ok(all) => all,
                Err(response) => return *response,
            };
            delete.extend(
                all.into_iter()
                    .filter(|m| m.retirement && m.supersedes.as_deref() == Some(memory.id.as_str()))
                    .map(|m| (RETIREMENT, m.id)),
            );
        }
        let kinds = vec![EventKind::MemoryForgotten {
            project: memory.project.clone(),
            memory: memory.id.clone(),
            by: actor.id,
        }];
        if let Some(error) = state.commit_change("memory", Vec::new(), delete, kinds) {
            return error;
        }
        Response::Ok
    }

    /// Retire a memory: keep the word that it is no longer true, with the
    /// reason, as a record of its own. Whoever works in the memory's
    /// project and can read it may, and the person: tidying a project's
    /// memory is not left to the sessions that happened to write it,
    /// which have usually ended. The event and the journal's line land
    /// with the record, or none of them does.
    pub(in crate::daemon) fn memory_retire(
        self: &Arc<Self>,
        from: &str,
        id: &str,
        reason: &str,
    ) -> Response {
        let mut state = lock(&self.state);
        let actor = match state.actor(from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if let Some(error) = state.write_failure() {
            return error;
        }
        let memory = match state.resolve_memory(id, actor.project.as_ref()) {
            Ok(memory) => memory,
            Err(response) => return *response,
        };
        if !actor.human && actor.project.as_ref() != Some(&memory.project) {
            return Response::error(
                ErrorCode::Forbidden,
                "a memory is retired by an agent of its own project, or by the person",
            );
        }
        let reading = match state.reading(Some(from), &memory.project) {
            Ok(reading) => reading,
            Err(response) => return *response,
        };
        // One its reader cannot read is, to that reader, not there.
        if !memory.seen_by(reading.reader()) {
            return Response::not_found(format!("no memory is {id}"));
        }
        let all = match state.project_memories(&memory.project) {
            Ok(all) => all,
            Err(response) => return *response,
        };
        let now = Utc::now();
        let said = memory::current(&all, reading.reader(), now);
        if !memory.retirement && !said.iter().any(|m| m.id == memory.id) {
            let why = match memory::superseded(&all, reading.reader(), now).get(memory.id.as_str())
            {
                Some(replaced) if replaced.retired.is_some() => format!(
                    "{} is already retired; forgetting the retirement {} would bring it back",
                    memory.id, replaced.by
                ),
                Some(replaced) => {
                    format!("{} was already replaced by {}", memory.id, replaced.by)
                }
                None => format!("{} is no longer said", memory.id),
            };
            return Response::error(ErrorCode::Invalid, why);
        }
        if all.iter().filter(|m| m.retirement).count() >= MEMORIES_PER_PROJECT {
            return Response::error(
                ErrorCode::Invalid,
                "a project keeps at most 1000 retirements; forget retired memories first",
            );
        }
        let retirement = match Memory::retiring(&memory, &actor.id, reason, now) {
            Ok(retirement) => retirement,
            Err(reason) => return Response::error(ErrorCode::Invalid, reason),
        };
        // The journal's line is the retiring agent's; the person's
        // retirement is told by its event alone, as the person's other
        // changes are.
        let previous_seq = state.journal_seq.clone();
        let entry = state
            .registry
            .resolve(&actor.id)
            .ok()
            .and_then(|id| state.registry.get(&id).cloned())
            .filter(|record| {
                record.project.as_ref().map(|p| p.id()) == Some(memory.project.clone())
            })
            .and_then(|record| {
                state.plain_entry(
                    &record,
                    JournalKind::Note,
                    retired_summary(&memory, &retirement),
                    SummarySource::Synthesised,
                )
            })
            .map(|mut entry| {
                entry.seq = state.next_journal_seq(&entry.project);
                entry
            });
        let entries: Vec<JournalEntry> = entry.into_iter().collect();
        let events: Vec<Event> = std::iter::once(EventKind::MemoryRetired {
            project: memory.project.clone(),
            memory: memory.id.clone(),
            retirement: retirement.id.clone(),
            by: actor.id,
        })
        .chain(entries.iter().map(|entry| EventKind::JournalAppended {
            entry: entry.clone(),
        }))
        .map(|kind| Event::new(kind, now))
        .collect();
        let documents = vec![memory_doc(&retirement)];
        let cached = entries.clone();
        let committed = state.commit(
            "memory retired",
            events,
            |store, events| store.documents_with_journal(&documents, &entries, events),
            |state, _, ()| {
                for entry in cached {
                    state.cache_journal(entry);
                }
            },
        );
        if committed != Persisted::Committed {
            // The journal's place was taken for a line that was not kept.
            state.journal_seq = previous_seq;
            return state.write_failure().unwrap_or_else(|| {
                Response::error(
                    ErrorCode::StorageUnavailable,
                    "the retirement was not recorded",
                )
            });
        }
        Response::Memory {
            memory: retirement,
            may_supersede: Vec::new(),
            suggestion: None,
            left_out: None,
        }
    }
}
