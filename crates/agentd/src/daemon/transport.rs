//! Runtime socket ownership; durable credentials remain separate from registry records.
use super::*;
use agentdocker_core::container::WorkspaceAccess;
use agentdocker_host::{containers::ContainerError, transport::Bridge};
use std::os::unix::fs::PermissionsExt;

pub(super) struct Transport {
    listener: Option<tokio::task::JoinHandle<()>>,
    bridge: Option<Bridge>,
}
impl Transport {
    fn alive(&mut self) -> bool {
        self.listener.as_ref().is_none_or(|t| !t.is_finished())
            && self.bridge.as_mut().is_none_or(Bridge::alive)
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        if let Some(listener) = &self.listener {
            listener.abort();
        }
    }
}
impl Daemon {
    /// Each container has its own mounted directory, so rebinding preserves visibility.
    pub(super) async fn ensure_transport(
        self: &Arc<Self>,
        record: &AgentRecord,
    ) -> Result<(), ContainerError> {
        let Some(access) = record
            .container
            .as_ref()
            .and_then(|c| c.workspace.as_ref())
            .and_then(|w| w.access.as_ref())
        else {
            return Ok(());
        };
        let socket = match self.restricted() {
            RestrictedEndpoint::On(socket) => socket,
            _ => {
                return Err(ContainerError::unavailable(
                    "authenticated workspace endpoint is not serving".into(),
                ));
            }
        };
        let old = {
            let mut state = lock(&self.state);
            if state
                .transports
                .get_mut(&record.id)
                .is_some_and(Transport::alive)
            {
                return Ok(());
            }
            state.transports.remove(&record.id)
        };
        drop(old);
        let mut transport = Transport {
            listener: None,
            bridge: None,
        };
        if access.relay.is_some() {
            transport.listener = Some(super::relay::start(record.clone(), socket).await?);
        } else if access.vm.is_none() {
            let path = access.socket_directory.join("endpoint.sock");
            if path.exists() {
                if tokio::net::UnixStream::connect(&path).await.is_ok() {
                    return Err(ContainerError::unavailable(
                        "workspace endpoint is already in use".into(),
                    ));
                }
                std::fs::remove_file(&path)
                    .map_err(|e| ContainerError::unavailable(e.to_string()))?;
            }
            let listener = tokio::net::UnixListener::bind(&path)
                .map_err(|e| ContainerError::unavailable(e.to_string()))?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| ContainerError::unavailable(e.to_string()))?;
            transport.listener = Some(tokio::spawn(async move {
                crate::server::serve_workspace(listener, socket).await;
            }));
        } else {
            let access: WorkspaceAccess = access.clone();
            transport.bridge = tokio::task::spawn_blocking(move || {
                agentdocker_host::transport::bridge(&access, &socket)
            })
            .await
            .map_err(|e| ContainerError::unavailable(e.to_string()))??;
        }
        lock(&self.state)
            .transports
            .insert(record.id.clone(), transport);
        Ok(())
    }
    pub(super) async fn retire_transport(&self, id: &AgentId) -> Result<(), ContainerError> {
        let old = lock(&self.state).transports.remove(id);
        drop(old);
        if let Some(record) = self
            .container_record(id)
            .filter(super::relay::needs_cleanup)
        {
            tokio::task::spawn_blocking(move || agentdocker_host::relay::cleanup(&record))
                .await
                .map_err(|e| ContainerError::unavailable(e.to_string()))??;
            self.update_container(id, |record| {
                record
                    .container
                    .as_mut()
                    .unwrap()
                    .workspace
                    .as_mut()
                    .unwrap()
                    .access
                    .as_mut()
                    .unwrap()
                    .relay
                    .as_mut()
                    .unwrap()
                    .retired = true
            })?;
        }
        Ok(())
    }
}
