//! Restricted endpoint credentials are hashed, scoped to one agent/checkout,
//! checked on every request, and never accepted on the host control endpoint.
use super::*;
use agentdocker_core::paths;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Serialize, Deserialize)]
struct Grant {
    id: String,
    token_hash: String,
    agent: AgentId,
    host_root: PathBuf,
    container_root: PathBuf,
    expires_at: DateTime<Utc>,
    revoked: bool,
}
fn denied(message: impl Into<String>) -> Response {
    Response::error(ErrorCode::Forbidden, message)
}
fn hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

impl Daemon {
    pub(super) fn grant_access(
        &self,
        reference: &str,
        container_root: String,
        ttl_secs: u64,
    ) -> Response {
        let (agent, host_root, _) = match self.reader_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        // A grant names the socket the container will use; without the
        // endpoint there is nothing to grant access to.
        let socket = match self.restricted() {
            RestrictedEndpoint::On(socket) => socket,
            RestrictedEndpoint::Starting => paths::container_socket(&self.home),
            RestrictedEndpoint::Off(reason) => {
                return Response::error(
                    ErrorCode::Unavailable,
                    format!("the container endpoint is off: {reason}"),
                );
            }
        };
        let container_root = PathBuf::from(container_root);
        if !container_root.is_absolute()
            || container_root
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            || !(1..=86400).contains(&ttl_secs)
        {
            return Response::error(
                ErrorCode::Invalid,
                "mapping needs an absolute container root and a TTL of 1–86400 seconds",
            );
        }
        let id = MessageId::generate().to_string();
        let token = format!("{}.{}{}", id, MessageId::generate(), MessageId::generate());
        let expires_at = Utc::now() + Duration::seconds(ttl_secs as i64);
        let grant = Grant {
            id: id.clone(),
            token_hash: hash(&token),
            agent: agent.clone(),
            host_root,
            container_root,
            expires_at,
            revoked: false,
        };
        let mut state = lock(&self.state);
        if let Err(e) = state.store.put_document("access", &id, &grant) {
            return Response::error(ErrorCode::Internal, e.to_string());
        }
        state.emit(EventKind::AccessGranted {
            agent,
            grant: id.clone(),
        });
        Response::Access {
            grant: id,
            token,
            socket,
            expires_at,
        }
    }

    pub(super) fn revoke_access(&self, id: &str) -> Response {
        let mut state = lock(&self.state);
        let mut grant = match state.store.document::<Grant>("access", id) {
            Ok(Some(g)) => g,
            _ => return denied("unknown grant"),
        };
        grant.revoked = true;
        if let Err(e) = state.store.put_document("access", id, &grant) {
            return Response::error(ErrorCode::Internal, e.to_string());
        }
        state.emit(EventKind::AccessRevoked { grant: id.into() });
        // Revocation denies new operations; it must not drop a running writer's leases.
        Response::Ok
    }

    pub fn restricted_request(
        &self,
        token: &str,
        mut request: Request,
    ) -> Result<Request, Box<Response>> {
        let reject = |message: &str| Box::new(denied(message));
        let id = token
            .split_once('.')
            .map(|v| v.0)
            .ok_or_else(|| reject("invalid credentials"))?;
        let state = lock(&self.state);
        let grant = state
            .store
            .document::<Grant>("access", id)
            .ok()
            .flatten()
            .ok_or_else(|| reject("invalid credentials"))?;
        let supplied = hash(token);
        let difference = supplied
            .bytes()
            .zip(grant.token_hash.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b));
        if supplied.len() != grant.token_hash.len()
            || difference != 0
            || grant.revoked
            || grant.expires_at <= Utc::now()
            || !state
                .registry
                .get(&grant.agent)
                .is_some_and(|a| a.status == AgentStatus::Running)
        {
            return Err(reject("expired, revoked or inactive credentials"));
        }
        drop(state);
        let identity = grant.agent.to_string();
        let check_agent = |input: &str| -> Result<(), Box<Response>> {
            if lock(&self.state).registry.resolve(input).ok().as_ref() == Some(&grant.agent) {
                Ok(())
            } else {
                Err(reject("credential cannot act as another agent"))
            }
        };
        let map = |raw: &str| -> Result<String, Box<Response>> {
            let path = Path::new(raw);
            if path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(reject("parent traversal is forbidden"));
            }
            let relative = if path.is_absolute() {
                path.strip_prefix(&grant.container_root)
                    .map_err(|_| reject("path is outside the mapped container checkout"))?
            } else {
                path
            };
            let physical = project::try_canonical(&grant.host_root.join(relative))
                .map_err(|_| reject("cannot resolve physical path"))?;
            if !physical.starts_with(&grant.host_root) {
                return Err(reject("mapped path escapes the checkout"));
            }
            Ok(physical.to_string_lossy().into_owned())
        };
        match &mut request {
            Request::Ping => {}
            Request::Inspect { agent }
            | Request::Reads { agent }
            | Request::Inbox { agent, .. }
            | Request::AckInbox { agent, .. }
            | Request::Release { agent, .. }
            | Request::ReleaseAll { agent, .. }
            | Request::JournalAdd { agent, .. } => {
                check_agent(agent)?;
                *agent = identity;
            }
            Request::Journal {
                project,
                path,
                digest,
                ..
            } => {
                let mapped = map(project)?;
                if Path::new(&mapped) != grant.host_root {
                    return Err(reject("journal project must be the mapped checkout root"));
                }
                *project = mapped;
                if let Some(path) = path {
                    *path = map(path)?;
                }
                if let Some(digest) = digest {
                    check_agent(&digest.reader)?;
                    digest.reader = identity;
                }
            }
            Request::Observe { agent, paths } | Request::Stale { agent, paths } => {
                check_agent(agent)?;
                *agent = identity;
                for path in paths {
                    *path = map(path)?;
                }
            }
            Request::Claim {
                agent,
                resource,
                wait_secs,
                ttl_secs,
                ..
            } => {
                check_agent(agent)?;
                *agent = identity;
                let path = resource
                    .strip_prefix("path:")
                    .ok_or_else(|| reject("restricted claims require mapped path resources"))?;
                *resource = format!("path:{}", map(path)?);
                // Do not allow a waiting request to acquire after credential revocation.
                *wait_secs = 0;
                *ttl_secs =
                    (*ttl_secs).min((grant.expires_at - Utc::now()).num_seconds().max(1) as u64);
            }
            Request::Renew {
                agent, ttl_secs, ..
            } => {
                check_agent(agent)?;
                *agent = identity;
                *ttl_secs =
                    (*ttl_secs).min((grant.expires_at - Utc::now()).num_seconds().max(1) as u64);
            }
            Request::Send { from, to, .. } => {
                check_agent(from)?;
                *from = identity;
                let state = lock(&self.state);
                let target = state
                    .registry
                    .resolve(to)
                    .map_err(|_| reject("restricted messaging needs a specific project peer"))?;
                let owner = state
                    .registry
                    .get(&grant.agent)
                    .and_then(|a| a.project.as_ref())
                    .map(ProjectRef::id);
                if state
                    .registry
                    .get(&target)
                    .and_then(|a| a.project.as_ref())
                    .map(ProjectRef::id)
                    != owner
                {
                    return Err(reject("recipient is outside the credential's project"));
                }
                *to = target.to_string();
            }
            _ => {
                return Err(reject(
                    "operation is unavailable on the restricted endpoint",
                ));
            }
        }
        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn credentials_scope_identity_paths_operations_and_revocation() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        let daemon =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        for name in ["worker", "peer"] {
            daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        name: name.into(),
                        workdir: Some(root.clone()),
                        ..AgentSpec::default()
                    },
                    pid: None,
                })
                .await;
        }
        let Response::Access { grant, token, .. } =
            daemon.grant_access("worker", "/workspace".into(), 60)
        else {
            panic!()
        };
        let journal = |project: &str, reader: &str| Request::Journal {
            project: project.into(),
            since_seq: None,
            until_seq: None,
            agent: None,
            branch: None,
            kind: None,
            path: None,
            grep: None,
            limit: 20,
            digest: Some(DigestRequest {
                reader: reader.into(),
                max_entries: 20,
                max_chars: 2000,
                all_branches: false,
                advance: true,
            }),
        };
        let scoped = daemon
            .restricted_request(&token, journal("/workspace", "worker"))
            .unwrap();
        assert!(matches!(
            daemon.handle(scoped).await,
            Response::Digest { .. }
        ));
        assert!(
            daemon
                .restricted_request(&token, journal("/outside", "worker"))
                .is_err()
        );
        assert!(
            daemon
                .restricted_request(&token, journal("/workspace", "peer"))
                .is_err()
        );
        assert!(
            daemon
                .restricted_request(
                    &token,
                    Request::JournalAdd {
                        agent: "peer".into(),
                        summary: "impersonation".into(),
                    }
                )
                .is_err()
        );
        assert!(
            daemon
                .restricted_request(
                    &token,
                    Request::JournalPrune {
                        project: "/workspace".into(),
                        before_seq: 10,
                    }
                )
                .is_err()
        );
        let claim = |agent: &str, path: &str| Request::Claim {
            agent: agent.into(),
            resource: format!("path:{path}"),
            mode: LeaseMode::Exclusive,
            ttl_secs: 300,
            note: None,
            wait_secs: 20,
        };
        assert!(daemon.restricted_request("wrong", Request::Ping).is_err());
        let original = lock(&daemon.state)
            .store
            .document::<Grant>("access", &grant)
            .unwrap()
            .unwrap();
        for length in [0, 63] {
            let mut corrupted = original.clone();
            corrupted.token_hash.truncate(length);
            lock(&daemon.state)
                .store
                .put_document("access", &grant, &corrupted)
                .unwrap();
            assert!(
                daemon.restricted_request(&token, Request::Ping).is_err(),
                "a truncated digest is never a valid credential"
            );
        }
        lock(&daemon.state)
            .store
            .put_document("access", &grant, &original)
            .unwrap();
        let outside = tmp.path().join("other-project");
        std::fs::create_dir(&outside).unwrap();
        daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "outsider".into(),
                    workdir: Some(outside),
                    ..AgentSpec::default()
                },
                pid: None,
            })
            .await;
        let send = |to: &str| Request::Send {
            from: "worker".into(),
            to: to.into(),
            kind: "chat".into(),
            payload: json!({"text":"hello"}),
            reply_to: None,
        };
        assert!(daemon.restricted_request(&token, send("outsider")).is_err());
        let peer = daemon.resolve("peer").unwrap().to_string();
        assert!(
            matches!(daemon.restricted_request(&token, send("peer")).unwrap(), Request::Send { to, .. } if to == peer)
        );
        assert!(
            daemon
                .restricted_request(&token, Request::Shutdown)
                .is_err()
        );
        assert!(
            daemon
                .restricted_request(
                    &token,
                    Request::Validate {
                        agent: "worker".into(),
                        command: vec!["sh".into()],
                        timeout_secs: 1
                    }
                )
                .is_err()
        );
        assert!(
            daemon
                .restricted_request(&token, claim("peer", "/workspace/file"))
                .is_err()
        );
        assert!(
            daemon
                .restricted_request(&token, claim("worker", "/workspace/../escape"))
                .is_err()
        );
        std::os::unix::fs::symlink(tmp.path().join("outside"), root.join("link")).unwrap();
        assert!(
            daemon
                .restricted_request(&token, claim("worker", "/workspace/link/new"))
                .is_err(),
            "dangling symlinks cannot escape the mount mapping"
        );
        let mapped = daemon
            .restricted_request(&token, claim("worker", "/workspace/file"))
            .unwrap();
        assert!(
            matches!(&mapped, Request::Claim { resource, wait_secs: 0, .. } if resource == &format!("path:{}", project::canonical(&root.join("file")).display()))
        );
        assert!(matches!(
            daemon.handle(mapped).await,
            Response::Lease { .. }
        ));
        daemon.revoke_access(&grant);
        assert!(daemon.restricted_request(&token, Request::Ping).is_err());
        assert!(
            matches!(daemon.handle(Request::Leases { agent: Some("worker".into()), resource: None }).await, Response::Leases { leases } if leases.len() == 1),
            "revocation must not drop a live writer's protection"
        );
    }
}
