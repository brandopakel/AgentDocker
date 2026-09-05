//! Host-authorized image builds and atomic persistence of their provenance.
use super::*;
use agentdocker_core::{ImageBuild, ImageBuildSpec};
use agentdocker_host::engine::{self, EngineError};

impl Daemon {
    pub(super) async fn build_image(&self, spec: ImageBuildSpec) -> Response {
        let id = AgentId::generate().to_string();
        let built = tokio::task::spawn_blocking(move || engine::build(spec, id)).await;
        match built {
            Ok(Ok(build)) => lock(&self.state).save_image_build(build),
            Ok(Err(error)) => {
                let code = match &error {
                    EngineError::Input(_) => ErrorCode::Invalid,
                    EngineError::Unavailable(_) => ErrorCode::EngineUnavailable,
                    EngineError::Build(_) | EngineError::Evidence(_) => ErrorCode::BuildFailed,
                };
                Response::error(
                    code,
                    error.to_string().chars().take(8192).collect::<String>(),
                )
            }
            Err(error) => Response::error(ErrorCode::Internal, error.to_string()),
        }
    }

    pub(super) fn images(&self) -> Response {
        let state = lock(&self.state);
        match state.store.documents::<ImageBuild>("image_build", None) {
            Ok(builds) => Response::ImageBuilds { builds },
            Err(error) => Response::error(ErrorCode::StorageUnavailable, error.to_string()),
        }
    }
}

impl State {
    fn save_image_build(&mut self, build: ImageBuild) -> Response {
        let mut event = Event::new(
            EventKind::ImageBuilt {
                build: build.id.clone(),
                engine: build.spec.engine,
                image_id: build.image_id.clone(),
            },
            Utc::now(),
        );
        event.seq = self.next_seq;
        self.persist("image build", |store| {
            store.put_document_with_event("image_build", &build.id, &build, &event)
        });
        if let Some(error) = self.storage_failure() {
            return error;
        }
        self.next_seq += 1;
        let _ = self.events.send(event);
        Response::ImageBuild { build }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn build_provenance_and_event_commit_together_and_survive_restart() {
        for reject in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let daemon =
                Arc::new(Daemon::open(tmp.path().to_path_buf(), tmp.path().join("sock")).unwrap());
            let build = ImageBuild {
                id: "build".into(),
                spec: ImageBuildSpec {
                    engine: agentdocker_core::ContainerEngine::Docker,
                    connection: None,
                    context: "/checkout".into(),
                    recipe: "Containerfile".into(),
                    timeout_secs: 60,
                },
                captured_at: Utc::now(),
                finished_at: Utc::now(),
                context_version: "sha256:context".into(),
                recipe_version: "sha256:recipe".into(),
                image_id: format!("sha256:{}", "a".repeat(64)),
                client_version: "1".into(),
                server_version: Some("2".into()),
                os: "linux".into(),
                architecture: "arm64".into(),
                variant: None,
            };
            let mut live = daemon.subscribe_events();
            {
                let mut state = lock(&daemon.state);
                if reject {
                    state.store.reject_event_for_test("image_built");
                }
                let response = state.save_image_build(build.clone());
                if reject {
                    assert!(matches!(
                        response,
                        Response::Error {
                            code: ErrorCode::StorageUnavailable,
                            ..
                        }
                    ));
                } else {
                    assert!(matches!(response, Response::ImageBuild { .. }));
                }
            }
            assert_eq!(live.try_recv().is_ok(), !reject);
            drop(daemon);
            let daemon =
                Arc::new(Daemon::open(tmp.path().to_path_buf(), tmp.path().join("sock")).unwrap());
            let Response::ImageBuilds { builds } = daemon.handle(Request::Images).await else {
                panic!()
            };
            assert_eq!(builds, if reject { Vec::new() } else { vec![build] });
            assert_eq!(daemon.recent_events(100).len(), usize::from(!reject));
        }
    }
}
