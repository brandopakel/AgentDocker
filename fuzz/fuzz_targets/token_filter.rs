#![no_main]
use agentd::daemon::Daemon;
use agentdocker_core::{AgentSpec, ErrorCode, Request, Response};
use libfuzzer_sys::fuzz_target;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

struct Fixture {
    daemon: Arc<Daemon>,
    root: PathBuf,
    agent: String,
    peer: String,
    token: String,
    revoked_token: String,
}

fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        // The campaign driver owns and removes this private directory even
        // when libFuzzer aborts. No real user state or provider is consulted.
        let base = PathBuf::from(
            std::env::var_os("AGENTDOCKER_FUZZ_ROOT")
                .expect("run through scripts/verify.sh fuzz or set a private fixture root"),
        );
        let base = base.join("token-filter");
        std::fs::create_dir(&base).unwrap();
        let root = base.join("checkout");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let outside = base.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        let daemon = Arc::new(Daemon::open(base.join("state"), base.join("socket")).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let register =
            |name: &str, path: &Path| match runtime.block_on(daemon.handle(Request::Register {
                spec: AgentSpec {
                    name: name.into(),
                    workdir: Some(path.into()),
                    ..Default::default()
                },
                pid: None,
                session: None,
            })) {
                Response::Agent { agent } => agent.id.to_string(),
                _ => panic!("fixture registration failed"),
            };
        let agent = register("fixture-owner", &root);
        let peer = register("fixture-peer", &root);
        register("fixture-outsider", &outside);
        let grant = || match runtime.block_on(daemon.handle(Request::GrantAccess {
            agent: agent.clone(),
            container_root: "/workspace".into(),
            ttl_secs: 86400,
        })) {
            Response::Access { grant, token, .. } => (grant, token),
            _ => panic!("fixture grant failed"),
        };
        let (_, token) = grant();
        let (revoked, revoked_token) = grant();
        assert!(matches!(
            runtime.block_on(daemon.handle(Request::RevokeAccess { grant: revoked })),
            Response::Ok
        ));
        Fixture {
            daemon,
            root,
            agent,
            peer,
            token,
            revoked_token,
        }
    })
}

#[derive(Deserialize)]
struct Input {
    request: Request,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    revoked: bool,
}

fuzz_target!(|data: &[u8]| {
    let Ok(input) = serde_json::from_slice::<Input>(data) else {
        return;
    };
    let f = fixture();
    let token = input.token.as_deref().unwrap_or(if input.revoked {
        &f.revoked_token
    } else {
        &f.token
    });
    let original_ttl = match &input.request {
        Request::Claim { ttl_secs, .. } | Request::Renew { ttl_secs, .. } => Some(*ttl_secs),
        _ => None,
    };
    let result = f.daemon.restricted_request(token, input.request);
    if token != f.token {
        assert!(
            matches!(result, Err(response) if matches!(*response, Response::Error { code: ErrorCode::Forbidden, .. }))
        );
        return;
    }
    let Ok(request) = result else { return };
    let mapped = |path: &str| {
        let path = Path::new(path);
        assert!(path.is_absolute() && path.starts_with(&f.root));
        assert!(
            !path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        );
    };
    match request {
        Request::Ping => {}
        Request::Inspect { agent }
        | Request::Reads { agent }
        | Request::Inbox { agent, .. }
        | Request::AckInbox { agent, .. }
        | Request::Release { agent, .. }
        | Request::ReleaseAll { agent, .. }
        | Request::JournalAdd { agent, .. } => assert_eq!(agent, f.agent),
        Request::Observe { agent, paths } | Request::Stale { agent, paths } => {
            assert_eq!(agent, f.agent);
            for path in paths {
                mapped(&path);
            }
        }
        Request::Claim {
            agent,
            resource,
            wait_secs,
            ttl_secs,
            ..
        } => {
            assert_eq!(agent, f.agent);
            mapped(resource.strip_prefix("path:").unwrap());
            assert_eq!(wait_secs, 0);
            assert!(ttl_secs <= original_ttl.unwrap() && ttl_secs <= 86400);
        }
        Request::Renew {
            agent, ttl_secs, ..
        } => {
            assert_eq!(agent, f.agent);
            assert!(ttl_secs <= original_ttl.unwrap() && ttl_secs <= 86400);
        }
        Request::Send { from, to, .. } => {
            assert_eq!(from, f.agent);
            assert!(to == f.agent || to == f.peer);
        }
        Request::Journal {
            project,
            path,
            digest,
            ..
        } => {
            assert_eq!(Path::new(&project), f.root);
            if let Some(path) = path {
                mapped(&path);
            }
            if let Some(digest) = digest {
                assert_eq!(digest.reader, f.agent);
            }
        }
        _ => panic!("restricted endpoint accepted an operation outside the audited allowlist"),
    }
});
