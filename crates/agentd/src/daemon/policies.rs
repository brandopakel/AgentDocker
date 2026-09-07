//! Reading the policy files, and asking them before acting.
//!
//! The rules themselves are pure and live in core. This is the part that
//! touches the disk: where the files are, when they are re-read, and
//! which requests are put to them.
//!
//! Two files, and the asymmetry between them is the point. `policy.toml`
//! under the daemon's home belongs to whoever owns the machine.
//! `.agentdocker/policy.toml` in a checkout travels in a repository and
//! could be written by anyone who can open a pull request, so it may add
//! restrictions and never remove one — which is enforced in core by
//! asking the host first and treating its refusal as final.
//!
//! Regular files up to 1 MiB are checked on the daemon tick. Errors retain
//! the last good policy; an initially invalid file denies governed actions.
//! Policy state changes have durable replay events before publication.

use std::path::{Path, PathBuf};

use agentdocker_core::policy::{self, Policy, Ruling};
use agentdocker_host::policy_file::{self, ReadPolicy, Stamp};

use super::*;

#[derive(Debug)]
pub(super) struct LaunchDenied(pub Response);

impl std::fmt::Display for LaunchDenied {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Response::Error { message, .. } => out.write_str(message),
            _ => out.write_str("launch admission failed"),
        }
    }
}
impl std::error::Error for LaunchDenied {}

/// The last valid rules, read identity and current diagnostic for one scope.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Loaded {
    pub policy: Policy,
    stamp: Option<Stamp>,
    has_good: bool,
    error: Option<String>,
}

/// Read outside the state lock. Absence removes rules; errors never do.
fn reload(path: &Path, previous: &Loaded) -> Option<Loaded> {
    let stamp = previous
        .error
        .is_none()
        .then_some(previous.stamp.as_ref())
        .flatten();
    let result = policy_file::read_changed(path, stamp)
        .map_err(|error| format!("cannot read policy ({:?})", error.kind()))
        .and_then(|read| match read {
            ReadPolicy::Unchanged => Ok(previous.clone()),
            ReadPolicy::Absent => Ok(Loaded::default()),
            ReadPolicy::Text { stamp, text } => {
                let mut policy: Policy =
                    toml::from_str(&text).map_err(|_| "invalid policy TOML".to_owned())?;
                canonicalise_paths(&mut policy);
                Ok(Loaded {
                    policy,
                    stamp: Some(stamp),
                    has_good: true,
                    error: None,
                })
            }
        });
    let next = match result {
        Ok(next) => next,
        Err(error) => {
            let mut next = previous.clone();
            next.error = Some(error);
            next
        }
    };
    (next != *previous).then_some(next)
}

/// Rewrite the literal part of every `path:` pattern to the canonical
/// path, where one exists.
///
/// The daemon canonicalises a resource before it claims it, so the
/// action a rule is matched against says `/private/tmp/...` on macOS
/// while the file the person wrote says `/tmp/...`. A rule that silently
/// matched nothing would be the worst possible failure for a security
/// boundary — it reads as protection and is not — so the pattern is
/// brought to the same form the action will be in.
///
/// Only the part before the first wildcard is touched, because that is
/// the part that names a real place. A prefix that does not exist is
/// left alone: it may be a directory that is yet to be created, and
/// guessing at it would be worse than matching it literally.
fn canonicalise_paths(policy: &mut Policy) {
    for rule in &mut policy.rules {
        for pattern in rule.allow.iter_mut().chain(rule.deny.iter_mut()) {
            if let Some(canonical) = canonicalise_pattern(pattern) {
                *pattern = canonical;
            }
        }
    }
}

/// `claim:path:/tmp/x/**` → `claim:path:/private/tmp/x/**` on a machine
/// where that is what `/tmp` is.
fn canonicalise_pattern(pattern: &str) -> Option<String> {
    let (action, rest) = pattern.split_once(":path:")?;
    let wildcard = rest.find(['*', '?']).unwrap_or(rest.len());
    let (literal, tail) = rest.split_at(wildcard);
    // `/repo/src/` and `/repo/src` are the same place but not the same
    // pattern: the separator belongs to the glob that follows it, and a
    // `PathBuf` will not carry it. Remembered here and put back below.
    let separated = literal.ends_with('/');
    // The literal part may name a file that does not exist yet, so walk
    // up to the nearest ancestor that does — a rule about a migrations
    // directory is usually written before the directory is.
    let mut ancestor = Path::new(literal.trim_end_matches('/'));
    // Components are collected rather than joined as we go: joining an
    // empty path appends a separator, which is how this grew a `//`.
    let mut climbed: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = ancestor.canonicalize() {
            let mut resolved = real;
            for name in climbed.iter().rev() {
                resolved.push(name);
            }
            let text = resolved.to_str()?;
            let separator = if separated { "/" } else { "" };
            return Some(format!("{action}:path:{text}{separator}{tail}"));
        }
        let parent = ancestor.parent()?;
        climbed.push(ancestor.file_name()?.to_owned());
        ancestor = parent;
    }
}

impl Daemon {
    /// Load a newly encountered scope before its first governed action.
    pub(super) fn refresh_policy_for(&self, project: Option<&ProjectRef>) {
        let root = project.map(|project| project.dir().to_path_buf());
        let (host, previous) = {
            let state = lock(&self.state);
            (
                state.host_policy.clone(),
                root.as_ref().map(|root| {
                    state
                        .project_policies
                        .get(root)
                        .cloned()
                        .unwrap_or_default()
                }),
            )
        };
        let next_host = reload(&self.host_policy_path(), &host);
        let next_project = root
            .as_ref()
            .zip(previous.as_ref())
            .and_then(|(root, previous)| reload(&Self::project_policy_path(root), previous));
        let mut state = lock(&self.state);
        if let Some(next) = next_host {
            state.apply_policy(None, &host, next);
        }
        if let Some(next) = next_project {
            state.apply_policy(root, previous.as_ref().unwrap(), next);
        }
    }

    /// Resolve selectors and refuse before creating a worktree, process or engine object.
    pub(super) async fn admit_run(&self, record: &mut AgentRecord) -> Result<(), Box<Response>> {
        if record.spec.name.is_empty() {
            record.spec.name = default_name(&record.id);
        }
        record.project = self.project_for(record.spec.workdir.clone(), true).await;
        self.refresh_policy_for(record.project.as_ref());
        match lock(&self.state).run_refusal(record) {
            Some(response) => Err(Box::new(response)),
            None => Ok(()),
        }
    }
    /// The host's policy file.
    fn host_policy_path(&self) -> PathBuf {
        self.home.join("policy.toml")
    }

    /// A project's, inside its checkout.
    fn project_policy_path(root: &Path) -> PathBuf {
        root.join(".agentdocker").join("policy.toml")
    }

    /// Check the active scopes outside the state lock. Concurrent scans only
    /// publish if their previous snapshot is still current.
    pub fn reload_policies(self: &Arc<Self>) {
        let (previous_host, projects) = {
            let state = lock(&self.state);
            let roots: std::collections::BTreeSet<PathBuf> = state
                .registry
                .live()
                .filter_map(|a| a.project.as_ref().map(|p| p.dir().to_path_buf()))
                .collect();
            let projects: Vec<_> = roots
                .into_iter()
                .map(|root| {
                    let previous = state
                        .project_policies
                        .get(&root)
                        .cloned()
                        .unwrap_or_default();
                    (root, previous)
                })
                .collect();
            (state.host_policy.clone(), projects)
        };
        let host = reload(&self.host_policy_path(), &previous_host);
        let projects: Vec<_> = projects
            .into_iter()
            .filter_map(|(root, previous)| {
                reload(&Self::project_policy_path(&root), &previous)
                    .map(|next| (root, previous, next))
            })
            .collect();
        let mut state = lock(&self.state);
        if let Some(next) = host {
            state.apply_policy(None, &previous_host, next);
        }
        for (root, previous, next) in projects {
            state.apply_policy(Some(root), &previous, next);
        }
    }
}

impl State {
    pub(super) fn run_refusal(&mut self, record: &AgentRecord) -> Option<Response> {
        if let Some(error) = self.storage_failure() {
            return Some(error);
        }
        let action = format!("run:{}", record.spec.name);
        let ruling = self.permits_record(record, &action);
        (!ruling.is_allowed()).then(|| self.refuse(&record.id, &action, ruling))
    }
    /// Commit the observable policy transition before changing admission rules.
    fn apply_policy(&mut self, root: Option<PathBuf>, previous: &Loaded, next: Loaded) {
        let current = root
            .as_ref()
            .map(|root| self.project_policies.get(root).cloned().unwrap_or_default())
            .unwrap_or_else(|| self.host_policy.clone());
        if &current != previous || self.storage_error.is_some() {
            return;
        }
        self.emit(EventKind::PolicyUpdated {
            project: root.clone(),
            rules: next.policy.rules.len() as u64,
            quotas: next.policy.quota.len() as u64,
            error: next.error.clone(),
            using_last_good: next.error.is_some() && next.has_good,
        });
        if self.storage_error.is_some() {
            return;
        }
        if let Some(root) = root {
            self.project_policies.insert(root, next);
        } else {
            self.host_policy = next;
        }
    }

    /// Whether the policy permits this agent to do this.
    ///
    /// The action is a colon-joined string the rules glob against —
    /// `claim:path:/repo/src/lib.rs`, `run:reviewer`, `send:project:abc`
    /// — so a policy file reads like the command it is about.
    pub(super) fn permits(&self, agent: &AgentId, action: &str) -> Ruling {
        let Some(record) = self.registry.get(agent) else {
            // Nothing to match a rule against. The caller is about to
            // fail on the missing agent anyway.
            return Ruling::Allow;
        };
        self.permits_record(record, action)
    }

    pub(super) fn permits_record(&self, record: &AgentRecord, action: &str) -> Ruling {
        let project = record
            .project
            .as_ref()
            .and_then(|p| self.project_policies.get(p.dir()));
        for (scope, loaded) in std::iter::once(("host", &self.host_policy))
            .chain(project.map(|loaded| ("project", loaded)))
        {
            if !loaded.has_good && loaded.error.is_some() {
                return Ruling::Deny {
                    rule: format!("{scope} policy unavailable"),
                    reason: "policy could not be loaded; no valid rules available".to_owned(),
                };
            }
        }
        policy::check(
            &self.host_policy.policy,
            project.map(|loaded| &loaded.policy),
            record,
            action,
        )
    }

    /// Refuse, announce it, and say which rule refused.
    pub(super) fn refuse(&mut self, agent: &AgentId, action: &str, ruling: Ruling) -> Response {
        let Ruling::Deny { rule, reason } = ruling else {
            return Response::Ok;
        };
        warn!(agent = %agent.short(), action, rule, "refused by policy");
        self.emit(EventKind::PolicyDenied {
            agent: agent.clone(),
            action: action.to_owned(),
            rule: rule.clone(),
        });
        Response::Error {
            code: ErrorCode::Forbidden,
            message: format!("{reason}, by {rule}"),
            details: Some(json!({ "rule": rule, "action": action })),
        }
    }

    /// The capacity of a quota for this agent, host and project together.
    pub(super) fn quota_capacity(&self, agent: &AgentId, quota: &str) -> Option<u64> {
        let project = self
            .registry
            .get(agent)
            .and_then(|record| record.project.as_ref())
            .and_then(|p| self.project_policies.get(p.dir()))
            .map(|loaded| &loaded.policy);
        policy::capacity(&self.host_policy.policy, project, quota)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn restore_obeys_new_run_policy_before_execution() {
        let dir = tempfile::tempdir().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("host.sock")).unwrap());
        let marker = dir.path().join("executed");
        let mut record = AgentRecord::new(
            AgentSpec {
                name: "denied-restore".into(),
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    "printf executed > \"$1\"".into(),
                    "fixture".into(),
                    marker.to_string_lossy().into(),
                ],
                restore: true,
                ..Default::default()
            },
            true,
            Utc::now(),
        );
        record.status = AgentStatus::Running;
        lock(&daemon.state).insert_record(record.clone());
        daemon.save_restore_points();
        daemon.mark_exited(&record.id, AgentStatus::Exited { code: Some(1) });
        std::fs::write(
            daemon.home.join("policy.toml"),
            "[[rule]]\ndeny = [\"run:**\"]\n",
        )
        .unwrap();
        daemon.restore_agents().await;
        daemon.stop_all().await;
        assert!(!marker.exists());
        let current = lock(&daemon.state)
            .registry
            .get(&record.id)
            .unwrap()
            .clone();
        assert!(current.pid.is_none());
        assert!(
            matches!(&current.status, AgentStatus::Failed { reason } if reason.contains("run:denied-restore")),
            "{:?}",
            current.status
        );
    }

    #[tokio::test]
    async fn a_new_project_policy_applies_to_the_first_claim_without_a_tick() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("checkout");
        std::fs::create_dir_all(checkout.join(".agentdocker")).unwrap();
        std::fs::write(
            checkout.join(".agentdocker/policy.toml"),
            "[[rule]]\ndeny = [\"claim:**\"]\n",
        )
        .unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("host.sock")).unwrap());
        let response = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "first-project".into(),
                    workdir: Some(checkout),
                    ..Default::default()
                },
                pid: Some(std::process::id()),
                session: None,
            })
            .await;
        let Response::Agent { agent } = response else {
            panic!("{response:?}")
        };
        let response = daemon
            .handle(Request::Claim {
                agent: agent.id.to_string(),
                resource: "task:first".into(),
                mode: LeaseMode::Exclusive,
                amount: None,
                ttl_secs: 60,
                note: None,
                wait_secs: 0,
            })
            .await;
        assert!(
            matches!(
                response,
                Response::Error {
                    code: ErrorCode::Forbidden,
                    ..
                }
            ),
            "{response:?}"
        );
    }

    #[tokio::test]
    async fn denied_pane_launch_does_not_probe_or_start_tmux() {
        let dir = tempfile::tempdir().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("host.sock")).unwrap());
        std::fs::write(
            daemon.home.join("policy.toml"),
            "[[rule]]\ndeny = [\"run:**\"]\n",
        )
        .unwrap();
        let response = daemon
            .handle(Request::Run {
                spec: AgentSpec {
                    name: "denied-pane".into(),
                    command: vec!["definitely-no-such-agentdocker-fixture".into()],
                    workdir: Some(dir.path().to_path_buf()),
                    in_pane: true,
                    ..Default::default()
                },
            })
            .await;
        assert!(
            matches!(
                response,
                Response::Error {
                    code: ErrorCode::Forbidden,
                    ..
                }
            ),
            "{response:?}"
        );
        assert_eq!(lock(&daemon.state).registry.len(), 0);
    }

    #[tokio::test]
    async fn run_policy_refuses_before_a_native_command_executes() {
        let dir = tempfile::tempdir().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("host.sock")).unwrap());
        std::fs::write(
            daemon.home.join("policy.toml"),
            "[[rule]]\ndeny = [\"run:**\"]\n",
        )
        .unwrap();
        daemon.reload_policies();
        let marker = dir.path().join("executed");
        let response = daemon
            .handle(Request::Run {
                spec: AgentSpec {
                    name: "denied-launch".into(),
                    command: vec![
                        "sh".into(),
                        "-c".into(),
                        "printf executed > \"$1\"".into(),
                        "fixture".into(),
                        marker.to_string_lossy().into(),
                    ],
                    ..Default::default()
                },
            })
            .await;
        daemon.stop_all().await;
        assert!(
            matches!(
                response,
                Response::Error {
                    code: ErrorCode::Forbidden,
                    ..
                }
            ),
            "{response:?}"
        );
        assert!(
            !marker.exists(),
            "a refused command must execute no instruction"
        );
    }

    #[tokio::test]
    async fn initial_invalid_policy_denies_claims_and_recovery_has_a_durable_event() {
        let dir = tempfile::tempdir().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("host.sock")).unwrap());
        let mut record = AgentRecord::new(
            AgentSpec {
                name: "policy-fixture".into(),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        record.status = AgentStatus::Running;
        assert!(matches!(
            lock(&daemon.state).insert_record(record.clone()),
            Response::Agent { .. }
        ));
        let request = || Request::Claim {
            agent: record.id.to_string(),
            resource: "task:protected".into(),
            mode: LeaseMode::Exclusive,
            amount: None,
            ttl_secs: 60,
            note: None,
            wait_secs: 0,
        };
        std::fs::write(daemon.home.join("policy.toml"), "[[rule] broken").unwrap();
        let mut events = daemon.subscribe_events();
        daemon.reload_policies();
        let first = events.try_recv().unwrap();
        assert!(matches!(
            first.kind,
            EventKind::PolicyUpdated {
                error: Some(_),
                using_last_good: false,
                ..
            }
        ));
        assert!(first.seq > 0);
        assert!(matches!(
            daemon.handle(request()).await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
        std::fs::write(daemon.home.join("policy.toml"), "").unwrap();
        daemon.reload_policies();
        assert!(matches!(
            daemon.handle(request()).await,
            Response::Lease { .. }
        ));
        assert!(
            daemon
                .recent_events(100)
                .iter()
                .any(|event| matches!(event.kind, EventKind::PolicyUpdated { error: None, .. }))
        );
    }

    #[test]
    fn failed_policy_event_keeps_rules_and_disables_coordination() {
        let dir = tempfile::tempdir().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("host.sock")).unwrap());
        let path = daemon.home.join("policy.toml");
        std::fs::write(&path, "[[rule]]\ndeny = [\"claim:**\"]\n").unwrap();
        daemon.reload_policies();
        let previous = lock(&daemon.state).host_policy.clone();
        lock(&daemon.state)
            .store
            .reject_event_for_test("policy_updated");
        std::fs::remove_file(&path).unwrap();
        daemon.reload_policies();
        let state = lock(&daemon.state);
        assert_eq!(state.host_policy, previous);
        assert!(state.storage_error.is_some());
    }

    #[test]
    fn a_metadata_error_must_not_erase_the_last_good_policy() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("policy.toml");
        std::fs::write(&path, "[[rule]]\ndeny = [\"claim:**\"]\n").unwrap();
        let loaded = reload(&path, &Loaded::default()).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&path, &path).unwrap();
        let next = reload(&path, &loaded).unwrap_or_else(|| loaded.clone());
        assert_eq!(next.policy, loaded.policy, "ELOOP is not policy deletion");
    }

    /// The daemon canonicalises a resource before claiming it, so a rule
    /// written against `/tmp` would silently match nothing on a machine
    /// where that is really `/private/tmp`. A rule that quietly protects
    /// nothing is the worst failure a policy can have — it reads as
    /// protection — so the pattern is brought to the same form first.
    #[test]
    fn a_path_pattern_is_brought_to_the_form_the_action_will_be_in() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let real = root.canonicalize().unwrap();
        // Only interesting where the two differ, which is the case this
        // exists for; elsewhere it is a no-op and still correct.
        let pattern = format!("claim:path:{}/migrations/**", root.display());
        let canonical = canonicalise_pattern(&pattern).expect("a path pattern");
        assert_eq!(
            canonical,
            format!("claim:path:{}/migrations/**", real.display())
        );
        assert!(agentdocker_core::policy::matches(
            &canonical,
            &format!("claim:path:{}/migrations/001.sql", real.display())
        ));
    }

    #[test]
    fn a_directory_that_does_not_exist_yet_still_resolves_through_one_that_does() {
        let dir = tempfile::TempDir::new().unwrap();
        let real = dir.path().canonicalize().unwrap();
        // `migrations` has not been created. The rule still has to be
        // about the right place, because it is usually written before
        // the directory is.
        let pattern = format!("claim:path:{}/not/here/yet/**", dir.path().display());
        let canonical = canonicalise_pattern(&pattern).unwrap();
        assert_eq!(
            canonical,
            format!("claim:path:{}/not/here/yet/**", real.display())
        );
    }

    #[test]
    fn patterns_that_are_not_paths_are_left_exactly_as_written() {
        assert_eq!(canonicalise_pattern("claim:task:migrations"), None);
        assert_eq!(canonicalise_pattern("send:project:*"), None);
        assert_eq!(canonicalise_pattern("run:**"), None);
        // A path whose leading directories do not exist still resolves
        // through the root, and keeps every component it was given: the
        // place may be created later, and the rule has to be about it.
        assert_eq!(
            canonicalise_pattern("claim:path:/nonexistent-root-xyz/**").as_deref(),
            Some("claim:path:/nonexistent-root-xyz/**")
        );
    }

    #[test]
    fn a_policy_that_will_not_parse_leaves_the_last_one_in_force() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("policy.toml");
        std::fs::write(&path, "[[rule]]\ndeny = [\"claim:**\"]\n").unwrap();
        let loaded = reload(&path, &Loaded::default()).expect("a first load");
        assert_eq!(loaded.policy.rules.len(), 1);

        // A typo must not read as "no rules": an empty policy allows
        // everything, so it would switch off exactly what it was
        // written to enforce.
        std::fs::write(&path, "[[rule]\nthis is not toml").unwrap();
        let broken = reload(&path, &loaded).expect("error becomes visible");
        assert_eq!(broken.policy, loaded.policy);
        assert!(broken.error.is_some());
        assert!(broken.has_good);

        // Removing it is a decision, and does take effect.
        std::fs::remove_file(&path).unwrap();
        let gone = reload(&path, &loaded).expect("removal is a change");
        assert!(gone.policy.rules.is_empty());
        assert!(gone.stamp.is_none());
    }
}
