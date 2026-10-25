//! A project's memory: documents of their own kind, by project, kept on
//! this machine. Any agent of the project or the person adds one; its
//! author or the person forgets it.
use super::*;
use agentdocker_core::memory::{self, MEMORIES_PER_PROJECT, Memory, Reader};

pub(in crate::daemon) const MEMORY: &str = "memory";

fn memory_doc(memory: &Memory) -> Doc {
    doc(MEMORY, memory.id.clone(), memory)
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

impl State {
    fn reading(&mut self, from: Option<&str>) -> Result<Reading, Box<Response>> {
        let Some(from) = from else {
            return Ok(Reading::Holding(Vec::new()));
        };
        let actor = self.actor(from)?;
        if actor.human {
            return Ok(Reading::Person);
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

    /// Every memory of a project, superseded ones included.
    pub(in crate::daemon) fn project_memories(
        &mut self,
        project: &ProjectId,
    ) -> Result<Vec<Memory>, Box<Response>> {
        self.store_read("memories", |store| {
            store.documents_where::<Memory>(
                MEMORY,
                "project",
                project.as_str(),
                MEMORIES_PER_PROJECT,
            )
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
        if let Some(Some(found)) =
            self.store_read("memory", |store| store.document::<Memory>(MEMORY, words))
        {
            return Ok(found);
        }
        let Some(project) = project else {
            return Err(not_found(format!("no memory is {words}")));
        };
        let memories = self.project_memories(project)?;
        let matches: Vec<&Memory> = memories
            .iter()
            .filter(|m| words.len() >= 4 && m.id.starts_with(words))
            .collect();
        match matches.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(not_found(format!("no memory is {words}"))),
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
        let mut state = lock(&self.state);
        let actor = match state.actor(from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if let Some(error) = state.write_failure() {
            return error;
        }
        let existing = match state.project_memories(&project) {
            Ok(memories) => memories,
            Err(response) => return *response,
        };
        if existing.len() >= MEMORIES_PER_PROJECT {
            return Response::error(
                ErrorCode::Invalid,
                "a project keeps at most 1000 memories; forget some first",
            );
        }
        // The roles it is said to are the project's own.
        if !roles.is_empty() {
            let catalog = match state.project_roles(&project) {
                Ok(catalog) => catalog,
                Err(response) => return *response,
            };
            let unknown: Vec<&str> = roles
                .iter()
                .filter(|r| !catalog.iter().any(|c| &c.role == *r))
                .map(String::as_str)
                .collect();
            if !unknown.is_empty() {
                let names: Vec<&str> = catalog.iter().map(|r| r.role.as_str()).collect();
                return Response::Error {
                    code: ErrorCode::Invalid,
                    message: format!(
                        "the project has no role called {}; its roles are {}",
                        unknown.join(", "),
                        names.join(", ")
                    ),
                    details: Some(serde_json::json!({ "roles": names })),
                };
            }
        }
        let reading = match state.reading(Some(from)) {
            Ok(reading) => reading,
            Err(response) => return *response,
        };
        // What it replaces is one of this project's, by its full id, and
        // one its author may read.
        let supersedes = match supersedes {
            Some(words) => match state.resolve_memory(&words, Some(&project)) {
                Ok(old) if old.project == project && !old.seen_by(reading.reader()) => {
                    return Response::error(ErrorCode::NotFound, format!("no memory is {words}"));
                }
                Ok(old) if old.project == project => Some(old.id),
                Ok(_) => {
                    return Response::error(
                        ErrorCode::Invalid,
                        "a memory replaces one of its own project's",
                    );
                }
                Err(response) => return *response,
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
            Err(reason) => return Response::error(ErrorCode::Invalid, reason),
        };
        let kinds = vec![EventKind::MemoryAdded {
            project,
            memory: memory.id.clone(),
            by: actor.id,
        }];
        if let Some(error) = state.commit_documents("memory", vec![memory_doc(&memory)], kinds) {
            return error;
        }
        Response::Memory { memory }
    }

    pub(in crate::daemon) async fn memory_search(
        self: &Arc<Self>,
        from: Option<String>,
        project: Option<String>,
        query: &str,
        limit: Option<usize>,
        as_of: Option<chrono::DateTime<Utc>>,
        include_superseded: bool,
    ) -> Response {
        let project = match self.memory_project(from.as_deref(), project).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        let (memories, reading) = {
            let mut state = lock(&self.state);
            let reading = match state.reading(from.as_deref()) {
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
            let (found, replaced) =
                memory::search_history(&memories, query, limit, reading.reader(), at);
            return Response::MemoryHistory {
                memories: found.into_iter().cloned().collect(),
                replaced,
            };
        }
        Response::Memories {
            memories: memory::search(&memories, query, limit, reading.reader(), at)
                .into_iter()
                .cloned()
                .collect(),
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
            let reading = match state.reading(from.as_deref()) {
                Ok(reading) => reading,
                Err(response) => return *response,
            };
            match state.project_memories(&project) {
                Ok(memories) => (memories, reading),
                Err(response) => return *response,
            }
        };
        Response::Memories {
            memories: memory::profile(
                &memories,
                max_chars.unwrap_or(4000).clamp(1, 64_000),
                reading.reader(),
                as_of.map_or_else(Utc::now, |at| at.min(Utc::now())),
            )
            .into_iter()
            .cloned()
            .collect(),
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
        // whether it names the memory by its full id or a prefix.
        if !actor.human
            && (memory.author != actor.id || actor.project.as_ref() != Some(&memory.project))
        {
            return Response::error(
                ErrorCode::Forbidden,
                "a memory is forgotten by whoever kept it, in its own project, or by the person",
            );
        }
        let kinds = vec![EventKind::MemoryForgotten {
            project: memory.project.clone(),
            memory: memory.id.clone(),
            by: actor.id,
        }];
        if let Some(error) =
            state.commit_change("memory", Vec::new(), vec![(MEMORY, memory.id)], kinds)
        {
            return error;
        }
        Response::Ok
    }
}
