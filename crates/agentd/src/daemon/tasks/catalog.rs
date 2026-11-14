//! A project's tags, and finding cards by words. An agent tags a card
//! only with a tag the project has; the person's new words become tags
//! as they use them, coloured round the palette.
use super::*;
use agentdocker_core::tag::{self, Tag, TagColor};

pub(in crate::daemon) const TAG: &str = "tag";

fn tag_doc(tag: &Tag) -> Doc {
    doc(TAG, tag::tag_id(&tag.project, &tag.name), tag)
}

impl State {
    pub(super) fn project_tags(&mut self, project: &ProjectId) -> Vec<Tag> {
        let mut tags = self
            .store_read("tags", |store| {
                store.documents_where::<Tag>(
                    TAG,
                    "project",
                    project.as_str(),
                    tag::TAGS_PER_PROJECT,
                )
            })
            .unwrap_or_default();
        tags.sort_by(|a, b| a.name.cmp(&b.name));
        tags
    }

    /// The tags a card would carry, against the project's catalog: the
    /// person's new words are tags to make (handed back as documents to
    /// write with the card); an agent's are refused, naming the catalog.
    pub(super) fn tags_for(
        &mut self,
        project: &ProjectId,
        actor: &Actor,
        wanted: &[String],
        now: DateTime<Utc>,
    ) -> Result<Vec<Doc>, Box<Response>> {
        let wanted: Vec<String> = wanted.iter().map(|t| task_tag(t)).collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let catalog = self.project_tags(project);
        let unknown: Vec<String> = tag::unknown(&wanted, &catalog)
            .into_iter()
            .map(str::to_owned)
            .collect();
        if unknown.is_empty() {
            return Ok(Vec::new());
        }
        if !actor.human {
            let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();
            return Err(Box::new(Response::Error {
                code: ErrorCode::Invalid,
                message: if names.is_empty() {
                    format!(
                        "the project has no tags yet; make {} with `create_tag` first",
                        unknown.join(", ")
                    )
                } else {
                    format!(
                        "no tag is called {}; the project's tags are {}",
                        unknown.join(", "),
                        names.join(", ")
                    )
                },
                details: Some(serde_json::json!({ "tags": names })),
            }));
        }
        if catalog.len() + unknown.len() > tag::TAGS_PER_PROJECT {
            return Err(Box::new(Response::invalid(
                "a project has at most 200 tags",
            )));
        }
        Ok(unknown
            .iter()
            .enumerate()
            .filter(|(_, name)| tag::valid(name))
            .map(|(index, name)| {
                tag_doc(&Tag {
                    project: project.clone(),
                    name: name.clone(),
                    color: TagColor::after(catalog.len() + index),
                    created_by: actor.id.clone(),
                    created_at: now,
                    updated_at: now,
                })
            })
            .collect())
    }
}

fn task_tag(words: &str) -> String {
    agentdocker_core::task::tag_of(words)
}

impl Daemon {
    pub(super) async fn named_project(
        self: &Arc<Self>,
        project: Option<&str>,
        from: Option<&str>,
    ) -> Result<ProjectId, Box<Response>> {
        if let Some(selector) = project {
            return self.resolve_project(selector).await;
        }
        let mut state = lock(&self.state);
        let actor = match from {
            Some(from) => Some(state.actor(from)?),
            None => None,
        };
        actor
            .and_then(|a| a.project)
            .ok_or_else(|| Box::new(Response::invalid("name a project")))
    }

    pub(in crate::daemon) async fn tags(self: &Arc<Self>, project: Option<String>) -> Response {
        let project = match self.named_project(project.as_deref(), None).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        Response::Tags {
            tags: lock(&self.state).project_tags(&project),
        }
    }

    /// Make a tag, or recolour or rename one; a rename follows it onto
    /// every card that has it, in the same commit.
    pub(in crate::daemon) async fn tag_save(
        self: &Arc<Self>,
        from: &str,
        project: Option<String>,
        name: &str,
        color: Option<TagColor>,
        rename: Option<String>,
    ) -> Response {
        let project = match self.named_project(project.as_deref(), Some(from)).await {
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
        let name = task_tag(name);
        if !tag::valid(&name) {
            return Response::error(
                ErrorCode::Invalid,
                "a tag is one to thirty-two letters, digits, -, _ or .",
            );
        }
        let now = Utc::now();
        let catalog = state.project_tags(&project);
        let existing = catalog.iter().find(|t| t.name == name).cloned();
        if existing.is_some() && rename.is_some() && !actor.human {
            return Response::error(ErrorCode::Forbidden, "renaming a tag is the person's to do");
        }
        let mut tag = match existing {
            Some(tag) => tag,
            None => {
                if catalog.len() >= tag::TAGS_PER_PROJECT {
                    return Response::error(ErrorCode::Invalid, "a project has at most 200 tags");
                }
                Tag {
                    project: project.clone(),
                    name: name.clone(),
                    color: TagColor::after(catalog.len()),
                    created_by: actor.id.clone(),
                    created_at: now,
                    updated_at: now,
                }
            }
        };
        if let Some(color) = color {
            tag.color = color;
        }
        tag.updated_at = now;
        let mut put = Vec::new();
        let mut delete = Vec::new();
        let renamed = rename.map(|r| task_tag(&r)).filter(|r| *r != tag.name);
        if let Some(new_name) = &renamed {
            if !tag::valid(new_name) {
                return Response::error(
                    ErrorCode::Invalid,
                    "a tag is one to thirty-two letters, digits, -, _ or .",
                );
            }
            if catalog.iter().any(|t| &t.name == new_name) {
                return Response::error(
                    ErrorCode::Conflict,
                    format!("the project has a tag called {new_name} already"),
                );
            }
            delete.push((TAG, tag::tag_id(&project, &tag.name)));
            let cards = match state.project_cards(&project) {
                Ok(cards) => cards,
                Err(response) => return *response,
            };
            for mut card in cards.into_iter().filter(|t| t.tags.contains(&tag.name)) {
                for word in &mut card.tags {
                    if *word == tag.name {
                        *word = new_name.clone();
                    }
                }
                card.tags.sort();
                card.tags.dedup();
                put.push(doc(DOCUMENT, card.id.to_string(), &card));
            }
            tag.name = new_name.clone();
        }
        put.push(tag_doc(&tag));
        let kinds = vec![EventKind::TagChanged {
            project: project.clone(),
            name: name.clone(),
            by: actor.id.clone(),
            renamed_to: renamed,
            deleted: false,
        }];
        if let Some(error) = state.commit_change("tag", put, delete, kinds) {
            return error;
        }
        Response::Tag { tag }
    }

    /// Delete a tag, and take it off every card: the person's to do.
    pub(in crate::daemon) async fn tag_delete(
        self: &Arc<Self>,
        from: &str,
        project: Option<String>,
        name: &str,
    ) -> Response {
        let project = match self.named_project(project.as_deref(), Some(from)).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        let mut state = lock(&self.state);
        let actor = match state.actor(from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if !actor.human {
            return Response::error(ErrorCode::Forbidden, "deleting a tag is the person's to do");
        }
        if let Some(error) = state.write_failure() {
            return error;
        }
        let name = task_tag(name);
        if !state.project_tags(&project).iter().any(|t| t.name == name) {
            return Response::error(ErrorCode::NotFound, format!("no tag is called {name}"));
        }
        let cards = match state.project_cards(&project) {
            Ok(cards) => cards,
            Err(response) => return *response,
        };
        let put: Vec<Doc> = cards
            .into_iter()
            .filter(|t| t.tags.contains(&name))
            .map(|mut card| {
                card.tags.retain(|t| *t != name);
                doc(DOCUMENT, card.id.to_string(), &card)
            })
            .collect();
        let kinds = vec![EventKind::TagChanged {
            project: project.clone(),
            name: name.clone(),
            by: actor.id.clone(),
            renamed_to: None,
            deleted: true,
        }];
        if let Some(error) =
            state.commit_change("tag", put, vec![(TAG, tag::tag_id(&project, &name))], kinds)
        {
            return error;
        }
        Response::Ok
    }

    /// Cards that answer some words, best first.
    pub(in crate::daemon) async fn task_search(
        self: &Arc<Self>,
        query: &str,
        project: Option<String>,
        archived: bool,
        limit: usize,
    ) -> Response {
        if query.trim().chars().count() < 2 {
            return Response::error(ErrorCode::Invalid, "search for at least two characters");
        }
        let limit = limit.clamp(1, 100);
        let named = match &project {
            Some(selector) => match self.resolve_project(selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let mut state = lock(&self.state);
        let cards = match named {
            Some(project) => match state.project_cards(&project) {
                Ok(cards) => cards,
                Err(response) => return *response,
            },
            None => {
                match state.store_read("cards", |store| store.documents::<Task>(DOCUMENT, None)) {
                    Some(cards) => cards,
                    None => {
                        return state.storage_failure().unwrap_or_else(|| {
                            Response::error(
                                ErrorCode::StorageUnavailable,
                                "the cards could not be read",
                            )
                        });
                    }
                }
            }
        };
        let cards: Vec<Task> = cards
            .into_iter()
            .filter(|t| archived || t.archived_at.is_none())
            .collect();
        let tasks = agentdocker_core::search::rank(query, &cards, limit)
            .into_iter()
            .cloned()
            .collect();
        Response::Tasks { tasks, more: false }
    }
}
