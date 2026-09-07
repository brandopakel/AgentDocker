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
//! Files are re-read when their modification time changes, checked on
//! the daemon's existing one-second tick. That is cheap — a `stat` per
//! file — and it means editing a policy takes effect without restarting
//! anything or remembering a signal.
//!
//! A file that will not parse is **not** treated as empty. An empty
//! policy allows everything, so a typo would silently switch off every
//! rule in it; instead the last good version stays in force and the
//! problem is logged.

use std::path::{Path, PathBuf};

use agentdocker_core::policy::{self, Policy, Ruling};

use super::*;

/// A policy file, and what it looked like when it was read.
#[derive(Clone, Debug, Default)]
pub(super) struct Loaded {
    pub policy: Policy,
    /// `None` when there is no file. Distinguishes "nothing to read"
    /// from "read and empty", so a file appearing later is noticed.
    modified: Option<std::time::SystemTime>,
    present: bool,
}

/// Read a policy file if it has changed since `previous`.
///
/// Returns `None` when nothing has changed, so the common tick does no
/// work beyond a `stat`.
fn reload(path: &Path, previous: &Loaded) -> Option<Loaded> {
    let modified = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
    match (modified, previous.present) {
        // Unchanged.
        (Some(now), true) if Some(now) == previous.modified => None,
        // Still absent.
        (None, false) => None,
        // Gone: back to allowing everything, which is what no file means.
        (None, true) => {
            info!(path = %path.display(), "policy file removed");
            Some(Loaded::default())
        }
        (Some(modified), _) => {
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) => {
                    warn!(path = %path.display(), %error, "cannot read policy; keeping the last one");
                    return None;
                }
            };
            match toml::from_str::<Policy>(&text) {
                Ok(mut policy) => {
                    canonicalise_paths(&mut policy);
                    info!(
                        path = %path.display(),
                        rules = policy.rules.len(),
                        quotas = policy.quota.len(),
                        "policy loaded"
                    );
                    Some(Loaded {
                        policy,
                        modified: Some(modified),
                        present: true,
                    })
                }
                Err(error) => {
                    // Not treated as empty: an empty policy allows
                    // everything, so a typo would switch off every rule
                    // the file was written to enforce.
                    warn!(
                        path = %path.display(),
                        %error,
                        "policy file is not valid; keeping the last one in force"
                    );
                    None
                }
            }
        }
    }
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
    /// The host's policy file.
    fn host_policy_path(&self) -> PathBuf {
        self.home.join("policy.toml")
    }

    /// A project's, inside its checkout.
    fn project_policy_path(root: &Path) -> PathBuf {
        root.join(".agentdocker").join("policy.toml")
    }

    /// Re-read any policy file whose modification time has moved. Called
    /// from the daemon's own tick, so an edit takes effect within a
    /// second without a restart or a signal.
    pub fn reload_policies(self: &Arc<Self>) {
        let host_path = self.host_policy_path();
        let (previous_host, roots) = {
            let state = lock(&self.state);
            let roots: Vec<PathBuf> = state
                .registry
                .live()
                .filter_map(|a| a.project.as_ref().map(|p| p.dir().to_path_buf()))
                .collect();
            (state.host_policy.clone(), roots)
        };
        // Reading files is I/O and does not belong under the lock.
        let host = reload(&host_path, &previous_host);
        let mut projects: Vec<(PathBuf, Loaded)> = Vec::new();
        for root in roots {
            let previous = {
                let state = lock(&self.state);
                state
                    .project_policies
                    .get(&root)
                    .cloned()
                    .unwrap_or_default()
            };
            if let Some(loaded) = reload(&Self::project_policy_path(&root), &previous) {
                projects.push((root, loaded));
            }
        }
        if host.is_none() && projects.is_empty() {
            return;
        }
        let mut state = lock(&self.state);
        if let Some(host) = host {
            state.host_policy = host;
        }
        for (root, loaded) in projects {
            state.project_policies.insert(root, loaded);
        }
    }
}

impl State {
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
        let project = record
            .project
            .as_ref()
            .and_then(|p| self.project_policies.get(p.dir()))
            .map(|loaded| &loaded.policy);
        policy::check(&self.host_policy.policy, project, record, action)
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
        assert!(
            reload(&path, &loaded).is_none(),
            "a broken file changes nothing"
        );

        // Removing it is a decision, and does take effect.
        std::fs::remove_file(&path).unwrap();
        let gone = reload(&path, &loaded).expect("removal is a change");
        assert!(gone.policy.rules.is_empty());
        assert!(!gone.present);
    }
}
