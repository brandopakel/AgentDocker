//! A project's plans and files. Plans are documents (their Markdown in
//! the document); a file's bytes are kept apart from its description, so
//! a list of files never carries them. Agents read, write and link both;
//! deleting a file is its uploader's or the person's.
use super::*;
use agentdocker_core::plan::{self, Plan, PlanStatus, ProjectFile};
use base64::Engine;
use sha2::Digest;

pub(in crate::daemon) const PLAN: &str = "plan";
pub(in crate::daemon) const FILE: &str = "project_file";
const FILE_BODY: &str = "file_body";
const FILE_LINK: &str = "file_link";

/// A file's link to one card: its own document, so a card's files are
/// found by the card (`documents_where task`).
#[derive(serde::Serialize, serde::Deserialize)]
struct FileLink {
    file: String,
    task: TaskId,
    project: ProjectId,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct FileBody {
    base64: String,
}

fn plan_doc(plan: &Plan) -> Doc {
    doc(PLAN, plan.id.clone(), plan)
}

fn file_doc(file: &ProjectFile) -> Doc {
    doc(FILE, file.id.clone(), file)
}

fn link_id(file: &str, task: &TaskId) -> String {
    format!("{file}:{task}")
}

impl State {
    pub(super) fn project_plans(&mut self, project: &ProjectId) -> Vec<Plan> {
        let mut plans = self
            .store_read("plans", |store| {
                store.documents_where::<Plan>(
                    PLAN,
                    "project",
                    project.as_str(),
                    plan::PLANS_PER_PROJECT,
                )
            })
            .unwrap_or_default();
        plans.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
        plans
    }

    /// A plan of the project by id, a unique id prefix, or its title.
    pub(super) fn resolve_plan(
        &mut self,
        project: &ProjectId,
        words: &str,
    ) -> Result<Plan, Box<Response>> {
        let words = words.trim();
        let plans = self.project_plans(project);
        let matches: Vec<&Plan> = plans
            .iter()
            .filter(|p| {
                p.id == words
                    || (words.len() >= 4 && p.id.starts_with(words))
                    || p.title.eq_ignore_ascii_case(words)
            })
            .collect();
        match matches.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(not_found(format!("no plan of the project is {words}"))),
            _ => Err(Box::new(Response::error(
                ErrorCode::Ambiguous,
                format!("more than one plan answers {words}"),
            ))),
        }
    }

    pub(super) fn plan_by_id(&mut self, id: &str) -> Result<Plan, Box<Response>> {
        match self.store_read("plan", |store| store.document::<Plan>(PLAN, id)) {
            Some(Some(plan)) => Ok(plan),
            Some(None) => Err(not_found(format!("no plan is {id}"))),
            None => Err(Box::new(self.storage_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::StorageUnavailable, "the plan could not be read")
            }))),
        }
    }

    fn project_files(&mut self, project: &ProjectId) -> Vec<ProjectFile> {
        let mut files = self
            .store_read("files", |store| {
                store.documents_where::<ProjectFile>(
                    FILE,
                    "project",
                    project.as_str(),
                    plan::FILES_PER_PROJECT,
                )
            })
            .unwrap_or_default();
        files.sort_by_key(|f| f.name.to_lowercase());
        files
    }

    /// The files linked to a card.
    pub(super) fn card_files(&mut self, task: &TaskId) -> Vec<ProjectFile> {
        let links = self
            .store_read("file links", |store| {
                store.documents_where::<FileLink>(FILE_LINK, "task", task.as_str(), 500)
            })
            .unwrap_or_default();
        let mut files: Vec<ProjectFile> = links
            .iter()
            .filter_map(|link| {
                self.store_read("file", |store| {
                    store.document::<ProjectFile>(FILE, &link.file)
                })
                .flatten()
            })
            .collect();
        files.sort_by_key(|f| f.name.to_lowercase());
        files
    }

    /// A file by id, a unique id prefix, or (within a project) its name.
    fn resolve_file(
        &mut self,
        words: &str,
        project: Option<&ProjectId>,
    ) -> Result<ProjectFile, Box<Response>> {
        let words = words.trim();
        if let Some(Some(file)) =
            self.store_read("file", |store| store.document::<ProjectFile>(FILE, words))
        {
            return Ok(file);
        }
        let candidates: Vec<ProjectFile> = match project {
            Some(project) => self.project_files(project),
            None => self
                .store_read("files", |store| store.documents::<ProjectFile>(FILE, None))
                .unwrap_or_default(),
        };
        let matches: Vec<&ProjectFile> = candidates
            .iter()
            .filter(|f| (words.len() >= 4 && f.id.starts_with(words)) || f.name == words)
            .collect();
        match matches.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(not_found(format!("no file is {words}"))),
            _ => Err(Box::new(Response::error(
                ErrorCode::Ambiguous,
                format!("more than one file answers {words}; give its id"),
            ))),
        }
    }
}

/// A file to keep: its name and type, its content as text or base64, and
/// the card it is for.
pub(in crate::daemon) struct FilePutArgs {
    pub name: String,
    pub media_type: Option<String>,
    pub text: Option<String>,
    pub base64: Option<String>,
    pub task: Option<String>,
}

impl Daemon {
    pub(in crate::daemon) async fn plans(
        self: &Arc<Self>,
        project: Option<String>,
        archived: bool,
    ) -> Response {
        let Some(selector) = project else {
            return Response::error(ErrorCode::Invalid, "name a project");
        };
        let project = match self.resolve_project(&selector).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        let plans = lock(&self.state)
            .project_plans(&project)
            .into_iter()
            .filter(|p| archived || p.status != PlanStatus::Archived)
            .map(|p| p.summary())
            .collect();
        Response::Plans { plans }
    }

    pub(in crate::daemon) fn plan_get(self: &Arc<Self>, id: &str) -> Response {
        let mut state = lock(&self.state);
        let plan = match state.plan_by_id(id) {
            Ok(plan) => plan,
            Err(response) => return *response,
        };
        let tasks = state
            .project_cards(&plan.project)
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t.plan.as_deref() == Some(plan.id.as_str()) && t.archived_at.is_none())
            .collect();
        Response::Plan { plan, tasks }
    }

    pub(in crate::daemon) async fn plan_save(
        self: &Arc<Self>,
        from: &str,
        project: Option<String>,
        id: Option<String>,
        title: Option<String>,
        body: Option<String>,
        status: Option<PlanStatus>,
    ) -> Response {
        let named = match &project {
            Some(selector) => match self.resolve_project(selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let mut state = lock(&self.state);
        let actor = match state.actor(from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if let Some(error) = state.write_failure() {
            return error;
        }
        let now = Utc::now();
        let mut plan = match id {
            Some(id) => match state.plan_by_id(&id) {
                Ok(plan) => plan,
                Err(response) => return *response,
            },
            None => {
                let Some(project) = named.or(actor.project.clone()) else {
                    return Response::error(ErrorCode::Invalid, "name a project");
                };
                if state.project_plans(&project).len() >= plan::PLANS_PER_PROJECT {
                    return Response::error(ErrorCode::Invalid, "a project has at most 500 plans");
                }
                let Some(title) = title.clone() else {
                    return Response::error(ErrorCode::Invalid, "a new plan needs a title");
                };
                match Plan::new(
                    project,
                    &title,
                    body.as_deref().unwrap_or(""),
                    &actor.id,
                    now,
                ) {
                    Ok(plan) => plan,
                    Err(reason) => return Response::error(ErrorCode::Invalid, reason),
                }
            }
        };
        if let Some(title) = title {
            plan.title = title.trim().to_owned();
        }
        if let Some(body) = body {
            plan.body = body;
        }
        if let Some(status) = status {
            plan.status = status;
        }
        plan.updated_at = now;
        if let Err(reason) = plan.check() {
            return Response::error(ErrorCode::Invalid, reason);
        }
        let kinds = vec![EventKind::PlanChanged {
            project: plan.project.clone(),
            plan: plan.id.clone(),
            by: actor.id.clone(),
        }];
        if let Some(error) = state.commit_documents("plan", vec![plan_doc(&plan)], kinds) {
            return error;
        }
        let tasks = state
            .project_cards(&plan.project)
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t.plan.as_deref() == Some(plan.id.as_str()) && t.archived_at.is_none())
            .collect();
        Response::Plan { plan, tasks }
    }

    pub(in crate::daemon) async fn files(
        self: &Arc<Self>,
        project: Option<String>,
        task: Option<String>,
    ) -> Response {
        if let Some(task) = task {
            let mut state = lock(&self.state);
            let task = match state.resolve_task(&task) {
                Ok(task) => task,
                Err(response) => return *response,
            };
            return Response::Files {
                files: state.card_files(&task.id),
            };
        }
        let Some(selector) = project else {
            return Response::error(ErrorCode::Invalid, "name a project or a card");
        };
        let project = match self.resolve_project(&selector).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        Response::Files {
            files: lock(&self.state).project_files(&project),
        }
    }

    /// Keep a file: a name the project has already is a new version of
    /// it, its links kept.
    pub(in crate::daemon) async fn file_put(
        self: &Arc<Self>,
        from: &str,
        project: Option<String>,
        file: FilePutArgs,
    ) -> Response {
        let FilePutArgs {
            name,
            media_type,
            text,
            base64,
            task,
        } = file;
        let name = name.as_str();
        if !plan::valid_file_name(name) {
            return Response::error(
                ErrorCode::Invalid,
                "a file's name is one to 200 characters without slashes",
            );
        }
        let bytes = match (text, base64) {
            (Some(text), None) => text.into_bytes(),
            (None, Some(encoded)) => {
                match base64::engine::general_purpose::STANDARD.decode(encoded.trim()) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        return Response::error(
                            ErrorCode::Invalid,
                            "the file's base64 does not decode",
                        );
                    }
                }
            }
            _ => {
                return Response::error(
                    ErrorCode::Invalid,
                    "give the file as `text` or as `base64`",
                );
            }
        };
        if bytes.len() > plan::FILE_BYTES {
            return Response::error(ErrorCode::Invalid, "a file is at most 4 MiB");
        }
        let named = match &project {
            Some(selector) => match self.resolve_project(selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let mut state = lock(&self.state);
        let actor = match state.actor(from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if let Some(error) = state.write_failure() {
            return error;
        }
        let card = match task.as_deref() {
            Some(words) => match state.resolve_task(words) {
                Ok(card) => Some(card),
                Err(response) => return *response,
            },
            None => None,
        };
        let Some(project) = named
            .or_else(|| card.as_ref().map(|c| c.project.clone()))
            .or(actor.project.clone())
        else {
            return Response::error(ErrorCode::Invalid, "name a project");
        };
        if card.as_ref().is_some_and(|c| c.project != project) {
            return Response::error(ErrorCode::Invalid, "the card is in another project");
        }
        let now = Utc::now();
        let name = name.trim().to_owned();
        let existing = state.project_files(&project);
        let mut file = match existing.iter().find(|f| f.name == name) {
            Some(file) => file.clone(),
            None => {
                if existing.len() >= plan::FILES_PER_PROJECT {
                    return Response::error(ErrorCode::Invalid, "a project has at most 500 files");
                }
                ProjectFile {
                    id: uuid::Uuid::new_v4().simple().to_string()[..12].to_owned(),
                    project: project.clone(),
                    name: name.clone(),
                    media_type: String::new(),
                    size: 0,
                    sha256: String::new(),
                    tasks: Vec::new(),
                    created_by: actor.id.clone(),
                    created_at: now,
                    updated_at: now,
                }
            }
        };
        file.media_type = media_type
            .map(|m| m.trim().to_owned())
            .filter(|m| !m.is_empty() && m.len() <= 128)
            .unwrap_or_else(|| plan::media_type_of(&name).to_owned());
        file.size = bytes.len() as u64;
        file.sha256 = format!("{:x}", sha2::Sha256::digest(&bytes));
        file.updated_at = now;
        let mut put = vec![doc(
            FILE_BODY,
            file.id.clone(),
            &FileBody {
                base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            },
        )];
        if let Some(card) = &card
            && !file.tasks.contains(&card.id)
        {
            file.tasks.push(card.id.clone());
            put.push(doc(
                FILE_LINK,
                link_id(&file.id, &card.id),
                &FileLink {
                    file: file.id.clone(),
                    task: card.id.clone(),
                    project: project.clone(),
                },
            ));
        }
        put.push(file_doc(&file));
        let kinds = vec![EventKind::ProjectFileChanged {
            project,
            file: file.id.clone(),
            by: actor.id.clone(),
            deleted: false,
        }];
        if let Some(error) = state.commit_change("file", put, Vec::new(), kinds) {
            return error;
        }
        Response::File { file }
    }

    pub(in crate::daemon) fn file_get(self: &Arc<Self>, id: &str) -> Response {
        let mut state = lock(&self.state);
        let file = match state.resolve_file(id, None) {
            Ok(file) => file,
            Err(response) => return *response,
        };
        let Some(Some(body)) = state.store_read("file body", |store| {
            store.document::<FileBody>(FILE_BODY, &file.id)
        }) else {
            return Response::error(ErrorCode::NotFound, "the file's bytes are gone");
        };
        if plan::is_text(&file.media_type)
            && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&body.base64)
            && let Ok(text) = String::from_utf8(bytes)
        {
            return Response::FileContent {
                file,
                text: Some(text),
                base64: None,
            };
        }
        Response::FileContent {
            file,
            text: None,
            base64: Some(body.base64),
        }
    }

    pub(in crate::daemon) fn file_link(
        self: &Arc<Self>,
        from: &str,
        id: &str,
        task: &str,
        unlink: bool,
    ) -> Response {
        let mut state = lock(&self.state);
        let actor = match state.actor(from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if let Some(error) = state.write_failure() {
            return error;
        }
        let card = match state.resolve_task(task) {
            Ok(card) => card,
            Err(response) => return *response,
        };
        let mut file = match state.resolve_file(id, Some(&card.project)) {
            Ok(file) => file,
            Err(response) => return *response,
        };
        if file.project != card.project {
            return Response::error(ErrorCode::Invalid, "the file is another project's");
        }
        let linked = file.tasks.contains(&card.id);
        if linked != unlink {
            return Response::File { file };
        }
        let mut put = Vec::new();
        let mut delete = Vec::new();
        if unlink {
            file.tasks.retain(|t| *t != card.id);
            delete.push((FILE_LINK, link_id(&file.id, &card.id)));
        } else {
            file.tasks.push(card.id.clone());
            put.push(doc(
                FILE_LINK,
                link_id(&file.id, &card.id),
                &FileLink {
                    file: file.id.clone(),
                    task: card.id.clone(),
                    project: card.project.clone(),
                },
            ));
        }
        file.updated_at = Utc::now();
        put.push(file_doc(&file));
        let kinds = vec![EventKind::ProjectFileChanged {
            project: file.project.clone(),
            file: file.id.clone(),
            by: actor.id.clone(),
            deleted: false,
        }];
        if let Some(error) = state.commit_change("file", put, delete, kinds) {
            return error;
        }
        Response::File { file }
    }

    pub(in crate::daemon) fn file_delete(self: &Arc<Self>, from: &str, id: &str) -> Response {
        let mut state = lock(&self.state);
        let actor = match state.actor(from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if let Some(error) = state.write_failure() {
            return error;
        }
        let file = match state.resolve_file(id, actor.project.as_ref()) {
            Ok(file) => file,
            Err(response) => return *response,
        };
        if !actor.human && file.created_by != actor.id {
            return Response::error(
                ErrorCode::Forbidden,
                "a file is deleted by whoever kept it, or by the person",
            );
        }
        let mut delete = vec![(FILE, file.id.clone()), (FILE_BODY, file.id.clone())];
        delete.extend(file.tasks.iter().map(|t| (FILE_LINK, link_id(&file.id, t))));
        let kinds = vec![EventKind::ProjectFileChanged {
            project: file.project.clone(),
            file: file.id.clone(),
            by: actor.id.clone(),
            deleted: true,
        }];
        if let Some(error) = state.commit_change("file", Vec::new(), delete, kinds) {
            return error;
        }
        Response::Ok
    }
}
