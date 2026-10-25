//! A project's memory: documents of their own kind, by project, kept on
//! this machine. Any agent of the project or the person adds one; its
//! author or the person forgets it.
use super::*;
use agentdocker_core::memory::{self, MEMORIES_PER_PROJECT, Memory};

pub(in crate::daemon) const MEMORY: &str = "memory";

fn memory_doc(memory: &Memory) -> Doc {
    doc(MEMORY, memory.id.clone(), memory)
}

impl State {
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
        // What it replaces is one of this project's, by its full id.
        let supersedes = match supersedes {
            Some(words) => match state.resolve_memory(&words, Some(&project)) {
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
    ) -> Response {
        let project = match self.memory_project(from.as_deref(), project).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        let memories = match lock(&self.state).project_memories(&project) {
            Ok(memories) => memories,
            Err(response) => return *response,
        };
        Response::Memories {
            memories: memory::search(
                &memories,
                query,
                limit.unwrap_or(20).clamp(1, 100),
                Utc::now(),
            )
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
    ) -> Response {
        let project = match self.memory_project(from.as_deref(), project).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        let memories = match lock(&self.state).project_memories(&project) {
            Ok(memories) => memories,
            Err(response) => return *response,
        };
        Response::Memories {
            memories: memory::profile(
                &memories,
                max_chars.unwrap_or(4000).clamp(1, 64_000),
                Utc::now(),
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
