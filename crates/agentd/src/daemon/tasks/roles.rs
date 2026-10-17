//! A project's roles: the five every project offers and the person's own,
//! each with a colour. The built-in five are never written until the person
//! changes one, so a project that never uses roles stores nothing, and a
//! built-in one taken out is kept as removed so it stays out. Reading the
//! palette changes nothing; only the person changes it.
use super::*;
use agentdocker_core::role::{self, RoleColor, RoleDef};

pub(in crate::daemon) const ROLE: &str = "role";

fn role_doc(role: &RoleDef) -> Doc {
    doc(ROLE, role::role_id(&role.project, &role.role), role)
}

/// When the built-in roles were "made": they were never made, and a time
/// that moved with every read would say they had just changed.
fn built_in() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH
}

impl State {
    /// What the store holds for a project's roles: the person's changes to
    /// the built-in ones, removals included, and their own.
    fn stored_roles(&mut self, project: &ProjectId) -> Vec<RoleDef> {
        self.store_read("roles", |store| {
            store.documents_where::<RoleDef>(
                ROLE,
                "project",
                project.as_str(),
                role::ROLES_PER_PROJECT,
            )
        })
        .unwrap_or_default()
    }

    pub(in crate::daemon) fn project_roles(&mut self, project: &ProjectId) -> Vec<RoleDef> {
        role::catalog(project, self.stored_roles(project), built_in())
    }
}

impl Daemon {
    pub(in crate::daemon) async fn role_catalog(
        self: &Arc<Self>,
        project: Option<String>,
    ) -> Response {
        let project = match self.named_project(project.as_deref(), None).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        Response::RoleCatalog {
            roles: lock(&self.state).project_roles(&project),
        }
    }

    /// Make a role, or rename, recolour or describe one: the person's to do.
    /// A built-in role the person took out comes back when they make it
    /// again.
    pub(in crate::daemon) async fn role_save(
        self: &Arc<Self>,
        from: &str,
        project: Option<String>,
        existing: Option<String>,
        name: &str,
        color: Option<RoleColor>,
        description: Option<String>,
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
            return Response::error(
                ErrorCode::Forbidden,
                "a project's roles are the person's to change",
            );
        }
        if let Some(error) = state.write_failure() {
            return error;
        }
        let name = name.trim().to_owned();
        let stored = state.stored_roles(&project);
        let catalog = role::catalog(&project, stored.clone(), built_in());
        let now = Utc::now();
        let mut def = match &existing {
            Some(address) => match catalog.iter().find(|r| &r.role == address) {
                Some(def) => def.clone(),
                None => {
                    return Response::error(
                        ErrorCode::NotFound,
                        format!("no role is called {address}"),
                    );
                }
            },
            None => {
                let Some(address) = role::slug_from(&name) else {
                    return Response::error(
                        ErrorCode::Invalid,
                        "a role's name needs a letter or a digit",
                    );
                };
                if catalog.iter().any(|r| r.role == address) {
                    return Response::error(
                        ErrorCode::Conflict,
                        format!("the project has a role called {address} already"),
                    );
                }
                // A built-in role the person took out is made again, not anew.
                match stored.iter().find(|r| r.role == address && r.removed) {
                    Some(removed) => RoleDef {
                        removed: false,
                        ..removed.clone()
                    },
                    None => {
                        if stored.len() >= role::ROLES_PER_PROJECT {
                            return Response::error(
                                ErrorCode::Invalid,
                                "a project has at most 24 roles",
                            );
                        }
                        RoleDef {
                            project: project.clone(),
                            role: address,
                            name: name.clone(),
                            color: RoleColor::after(catalog.len()),
                            description: String::new(),
                            removed: false,
                            created_by: actor.id.clone(),
                            created_at: now,
                            updated_at: now,
                        }
                    }
                }
            }
        };
        def.name = name;
        if let Some(color) = color {
            def.color = color;
        }
        if let Some(description) = description {
            def.description = description.trim().to_owned();
        }
        if let Err(reason) = role::check_words(&def.name, &def.description) {
            return Response::error(ErrorCode::Invalid, reason);
        }
        def.created_by = actor.id.clone();
        def.updated_at = now;
        let kinds = vec![EventKind::RoleChanged {
            project: project.clone(),
            role: def.role.clone(),
            by: actor.id.clone(),
            deleted: false,
        }];
        if let Some(error) = state.commit_change("role", vec![role_doc(&def)], Vec::new(), kinds) {
            return error;
        }
        Response::RoleDef { role: def }
    }

    /// Take a role out of the project's palette: the person's to do. A
    /// built-in one is kept as removed, so it stays out; the person's own
    /// is deleted.
    pub(in crate::daemon) async fn role_delete(
        self: &Arc<Self>,
        from: &str,
        project: Option<String>,
        address: &str,
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
            return Response::error(
                ErrorCode::Forbidden,
                "a project's roles are the person's to change",
            );
        }
        if let Some(error) = state.write_failure() {
            return error;
        }
        let Some(def) = state
            .project_roles(&project)
            .into_iter()
            .find(|r| r.role == address)
        else {
            return Response::error(ErrorCode::NotFound, format!("no role is called {address}"));
        };
        let built_in_role = role::defaults(&project, built_in())
            .iter()
            .any(|r| r.role == def.role);
        let (put, delete) = if built_in_role {
            let removed = RoleDef {
                removed: true,
                created_by: actor.id.clone(),
                updated_at: Utc::now(),
                ..def.clone()
            };
            (vec![role_doc(&removed)], Vec::new())
        } else {
            (Vec::new(), vec![(ROLE, role::role_id(&project, &def.role))])
        };
        let kinds = vec![EventKind::RoleChanged {
            project: project.clone(),
            role: def.role.clone(),
            by: actor.id.clone(),
            deleted: true,
        }];
        if let Some(error) = state.commit_change("role", put, delete, kinds) {
            return error;
        }
        // Nobody holds a role the project no longer has, and its room closes.
        if let Some(error) = state.retire_role(&project, &def.role) {
            return error;
        }
        Response::Ok
    }
}
