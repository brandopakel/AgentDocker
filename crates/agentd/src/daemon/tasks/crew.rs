//! A project's crew: the team's description, kept by the daemon.
//!
//! One document per project. Reading it changes nothing, and with none
//! saved a reader is shown the crew the daemon would propose from what is
//! installed. Only the person saves one: an edit names the revision it was
//! made against, and the first can be the proposal, accepted by the key it
//! was shown under. Nothing here starts a session.
use super::*;
use agentdocker_core::crew::{self, Crew, Known, MemberId, MemberInput, Refused};
use std::collections::BTreeSet;

pub(in crate::daemon) const CREW: &str = "crew";

impl State {
    /// The crew the store holds for a project, if any. A store that
    /// cannot say is a failure, never 'none saved': that would show a
    /// proposal over a team the person has.
    fn stored_crew(&mut self, project: &ProjectId) -> Result<Option<Crew>, Box<Response>> {
        self.store_read("crew", |store| {
            store.document::<Crew>(CREW, project.as_str())
        })
        .ok_or_else(|| {
            Box::new(self.storage_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::StorageUnavailable, "the team could not be read")
            }))
        })
    }

    /// The roles a member can be given: the project's palette as it is
    /// now, without the ones taken out.
    fn crew_palette(&mut self, project: &ProjectId) -> Result<BTreeSet<String>, Box<Response>> {
        Ok(self
            .project_roles(project)?
            .into_iter()
            .filter(|role| !role.removed)
            .map(|role| role.role)
            .collect())
    }

    /// Save `members` as the project's crew, as the person. The lock is
    /// held from the read of what is saved to its commit, so two saves
    /// against one revision cannot both be taken.
    fn save_crew(
        &mut self,
        from: &str,
        project: &ProjectId,
        expect: u64,
        members: Vec<MemberInput>,
    ) -> Result<Crew, Box<Response>> {
        let actor = self.actor(from)?;
        if !actor.human {
            return Err(Box::new(Response::error(
                ErrorCode::Forbidden,
                "a project's team is the person's to change",
            )));
        }
        if let Some(error) = self.write_failure() {
            return Err(Box::new(error));
        }
        let current = self.stored_crew(project)?;
        let palette = self.crew_palette(project)?;
        let saved = crew::save(
            project,
            current.as_ref(),
            expect,
            members,
            &palette,
            crew::Saving {
                by: &actor.id,
                at: Utc::now(),
            },
            MemberId::generate,
        )
        .map_err(|refused| {
            let code = match refused {
                Refused::Stale { .. } => ErrorCode::Conflict,
                Refused::Invalid(_) => ErrorCode::Invalid,
            };
            Box::new(Response::error(code, refused.words()))
        })?;
        let kinds = vec![EventKind::CrewSaved {
            project: project.clone(),
            revision: saved.revision,
            by: actor.id.clone(),
            members: saved.members.len() as u64,
        }];
        let put = vec![doc(CREW, project.as_str().to_owned(), &saved)];
        match self.commit_change("crew", put, Vec::new(), kinds) {
            Some(error) => Err(Box::new(error)),
            None => Ok(saved),
        }
    }
}

impl Daemon {
    /// What the inventory says of each runtime, read off the state lock.
    async fn crew_runtimes(self: &Arc<Self>) -> Result<Vec<Known>, Box<Response>> {
        let roots = self.runtime_roots();
        let inventory = tokio::task::spawn_blocking(move || {
            agentdocker_host::runtimes::inventory(&roots, "agentdocker")
        })
        .await;
        match inventory {
            Ok(Ok(runtimes)) => Ok(runtimes
                .into_iter()
                .map(|runtime| Known {
                    installed: runtime.cli.is_some(),
                    runtime: runtime.name,
                    vendor: runtime.vendor,
                })
                .collect()),
            Ok(Err(error)) => Err(Box::new(Response::error(
                ErrorCode::Unavailable,
                error.to_string(),
            ))),
            Err(_) => Err(Box::new(Response::error(
                ErrorCode::Unavailable,
                "runtime inventory worker failed",
            ))),
        }
    }

    pub(in crate::daemon) async fn crew(self: &Arc<Self>, project: Option<String>) -> Response {
        let project = match self.named_project(project.as_deref(), None).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        let known = match self.crew_runtimes().await {
            Ok(known) => known,
            Err(response) => return *response,
        };
        match lock(&self.state).stored_crew(&project) {
            Ok(saved) => Response::Crew {
                crew: crew::view(&project, saved.as_ref(), &known),
            },
            Err(response) => *response,
        }
    }

    /// Save a project's crew: the person's to do.
    pub(in crate::daemon) async fn crew_save(
        self: &Arc<Self>,
        from: &str,
        project: Option<String>,
        expect: u64,
        members: Vec<MemberInput>,
    ) -> Response {
        let project = match self.named_project(project.as_deref(), Some(from)).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        // Read before the lock: what is installed only decides how the
        // saved crew is shown, never what is saved.
        let known = match self.crew_runtimes().await {
            Ok(known) => known,
            Err(response) => return *response,
        };
        match lock(&self.state).save_crew(from, &project, expect, members) {
            Ok(saved) => Response::Crew {
                crew: crew::view(&project, Some(&saved), &known),
            },
            Err(response) => *response,
        }
    }

    /// Save the proposed crew as a project's first, named by the key the
    /// person was shown it under: the person's to do. What is installed
    /// is read again, and a proposal that is no longer the one for it is
    /// refused, so what is saved is what was shown.
    pub(in crate::daemon) async fn crew_accept(
        self: &Arc<Self>,
        from: &str,
        project: Option<String>,
        proposal: &str,
    ) -> Response {
        let project = match self.named_project(project.as_deref(), Some(from)).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        let known = match self.crew_runtimes().await {
            Ok(known) => known,
            Err(response) => return *response,
        };
        let mut state = lock(&self.state);
        match state.stored_crew(&project) {
            Ok(None) => {}
            Ok(Some(saved)) => {
                return Response::error(
                    ErrorCode::Conflict,
                    format!(
                        "the project has a team saved (revision {}); edit it rather than accept a proposal",
                        saved.revision
                    ),
                );
            }
            Err(response) => return *response,
        }
        let offered = crew::view(&project, None, &known);
        let Some(key) = offered.proposal.as_deref() else {
            return Response::error(
                ErrorCode::Unavailable,
                offered
                    .unavailable
                    .unwrap_or_else(|| crew::NOTHING_TO_PROPOSE.to_owned()),
            );
        };
        if key != proposal {
            return Response::error(
                ErrorCode::Conflict,
                format!(
                    "what is installed has changed since that team was proposed: the proposal is now `{key}`; read it again"
                ),
            );
        }
        let members = offered
            .members
            .iter()
            .map(|member| MemberInput {
                name: member.name.clone(),
                runtime: member.runtime.clone(),
                roles: member.roles.clone(),
                own_worktree: member.own_worktree,
                ..MemberInput::default()
            })
            .collect();
        match state.save_crew(from, &project, 0, members) {
            Ok(saved) => Response::Crew {
                crew: crew::view(&project, Some(&saved), &known),
            },
            Err(response) => *response,
        }
    }
}
