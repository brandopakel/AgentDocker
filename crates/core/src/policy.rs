//! What an agent is allowed to do, before it does it.
//!
//! Leases stop two agents editing one file. This stops one agent
//! touching something it was never meant to — the migrations directory,
//! the release branch, a project it has no business in. Docker calls the
//! same idea an authorization plugin; here it is a file the daemon reads
//! and a pure function it consults.
//!
//! Two files: `<home>/policy.toml` for the host, and
//! `<root>/.agentdocker/policy.toml` for a project. **A project cannot
//! widen the host.** The host file is the machine owner's; a project
//! file travels in a repository and could be written by anyone who can
//! open a pull request, so it may add restrictions and never remove one.
//! That asymmetry is the whole reason there are two files.
//!
//! Pure: no filesystem, no clock. The daemon reads the files and passes
//! what it read, so every rule about precedence is a unit test.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::AgentRecord;

/// A parsed policy file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
    /// Capacities for `quota:<name>` resources, by name. A quota with no
    /// capacity here is unlimited, so a typo loosens nothing that was
    /// not already loose.
    #[serde(default)]
    pub quota: BTreeMap<String, u64>,
    /// What the project does with what remote senders say to it.
    #[serde(default)]
    pub inbound: Inbound,
}

/// How a project treats a message from a class of sender before anyone
/// in it reads it.
///
/// Ordered from the most to the least trusting, so that the stricter of
/// two files is the greater: a project may hold or refuse what the host
/// accepts, and never accept what the host holds or refuses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    /// Delivered as any agent's message is.
    #[default]
    Accept,
    /// Kept for the person, who sees it among the questions put to them
    /// and releases it or drops it. Nothing reaches its recipients until
    /// they release it.
    Hold,
    /// Refused at the send, `forbidden`, naming the setting that refused.
    Refuse,
}

impl Trust {
    /// The word the policy file uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Hold => "hold",
            Self::Refuse => "refuse",
        }
    }
}

/// Inbound trust: what the project does with what a remote sender says.
///
/// Only senders that join through the remote connector are classed here:
/// a browser or hosted session of a Claude or ChatGPT account, which has no
/// process on this machine and whose word is attributed input the person
/// may want to see first. A session running here is not a remote sender
/// and is never held or refused by this section; the `[[rule]]` section
/// governs its `send:*` actions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inbound {
    /// The class every connector sender gets, unless a `sender` entry
    /// names it. Left out, a connector sender's word is accepted.
    #[serde(default)]
    pub connector: Option<Trust>,
    /// Classes for particular connector senders; the first entry that
    /// matches decides, before `connector`.
    #[serde(default, rename = "sender")]
    pub senders: Vec<InboundSender>,
}

/// One connector sender, or a set of them, and how to treat its word.
/// Every field left out matches every connector sender.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboundSender {
    /// A name for the entry, so a refusal can say which one refused.
    #[serde(default)]
    pub name: Option<String>,
    /// Glob over the sender's agent name (`claude-connector-*`).
    #[serde(default)]
    pub agent: Option<String>,
    /// The sender's runtime: the vendor account it came through
    /// (`claude-browser`, `chatgpt-browser`).
    #[serde(default)]
    pub runtime: Option<String>,
    pub trust: Trust,
}

impl InboundSender {
    fn covers(&self, sender: &AgentRecord) -> bool {
        if let Some(runtime) = &self.runtime
            && *runtime != sender.spec.runtime
        {
            return false;
        }
        if let Some(pattern) = &self.agent
            && !matches(pattern, &sender.spec.name)
        {
            return false;
        }
        true
    }

    fn label(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            let mut parts = Vec::new();
            if let Some(runtime) = &self.runtime {
                parts.push(format!("runtime={runtime}"));
            }
            if let Some(agent) = &self.agent {
                parts.push(format!("agent={agent}"));
            }
            if parts.is_empty() {
                "the entry for every connector sender".to_owned()
            } else {
                parts.join(" ")
            }
        })
    }
}

/// What inbound trust said of one sender, and which setting said it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundRuling {
    pub trust: Trust,
    /// The setting that decided (`host: inbound.connector`,
    /// `project: <entry name>`), or that nothing was written.
    pub rule: String,
}

impl InboundRuling {
    fn accepted() -> Self {
        Self {
            trust: Trust::Accept,
            rule: "nothing written".to_owned(),
        }
    }
}

/// One rule: who it is about, and what they may or may not do.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// A name for the rule, so a refusal can say which one refused.
    #[serde(default)]
    pub name: Option<String>,
    /// Which agents this is about. Every field left out matches
    /// everything, so a rule with no conditions is about everyone.
    #[serde(default)]
    pub runtime: Option<String>,
    /// Glob over the agent's name.
    #[serde(default)]
    pub agent: Option<String>,
    /// Project id prefix, or the project's directory name.
    #[serde(default)]
    pub project: Option<String>,
    /// Every one of these labels must be on the agent, with this value.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Action patterns this rule permits. A rule with any `allow` turns
    /// whitelisting on for the agents it matches: they may then do only
    /// what some matching rule allows.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Action patterns this rule forbids. A deny always wins.
    #[serde(default)]
    pub deny: Vec<String>,
}

/// What the policy said.
///
/// Named `Ruling` because a channel already has a `Decision` and a
/// review a `Verdict`; three words for three different judgements is
/// better than one word meaning three things.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Ruling {
    Allow,
    Deny {
        /// The rule that refused, by name where it has one.
        rule: String,
        reason: String,
    },
}

impl Ruling {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

impl Rule {
    /// Whether this rule is about this agent.
    pub fn covers(&self, agent: &AgentRecord) -> bool {
        if let Some(runtime) = &self.runtime
            && *runtime != agent.spec.runtime
        {
            return false;
        }
        if let Some(pattern) = &self.agent
            && !matches(pattern, &agent.spec.name)
        {
            return false;
        }
        if let Some(project) = &self.project {
            let own = agent.project.as_ref();
            let by_id = own.is_some_and(|p| p.id().as_str().starts_with(project.as_str()));
            let by_name = own.is_some_and(|p| p.name() == *project);
            if !by_id && !by_name {
                return false;
            }
        }
        // An agent may hold several roles: a rule on one covers each that
        // holds it.
        self.labels.iter().all(|(key, value)| {
            if key == crate::agent::ROLE_LABEL {
                agent.holds_role(value)
            } else {
                agent.spec.labels.get(key) == Some(value)
            }
        })
    }

    fn label(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            let mut parts = Vec::new();
            if let Some(runtime) = &self.runtime {
                parts.push(format!("runtime={runtime}"));
            }
            if let Some(agent) = &self.agent {
                parts.push(format!("agent={agent}"));
            }
            if let Some(project) = &self.project {
                parts.push(format!("project={project}"));
            }
            if parts.is_empty() {
                "the rule for everyone".to_owned()
            } else {
                parts.join(" ")
            }
        })
    }
}

impl Policy {
    /// Whether this policy, on its own, permits the action.
    ///
    /// Three steps, and the order is the point. A matching **deny**
    /// refuses outright, whatever else is written. Otherwise, if any
    /// rule that covers this agent carries an **allow** list, the agent
    /// is in whitelist mode and the action has to appear in one of them.
    /// Otherwise it is permitted: a machine with no policy file, or one
    /// that only denies, allows everything else.
    pub fn check(&self, agent: &AgentRecord, action: &str) -> Ruling {
        let covering: Vec<&Rule> = self.rules.iter().filter(|r| r.covers(agent)).collect();
        for rule in &covering {
            if rule.deny.iter().any(|pattern| matches(pattern, action)) {
                return Ruling::Deny {
                    rule: rule.label(),
                    reason: format!("`{action}` is denied"),
                };
            }
        }
        let whitelisting: Vec<&&Rule> = covering.iter().filter(|r| !r.allow.is_empty()).collect();
        if whitelisting.is_empty() {
            return Ruling::Allow;
        }
        if whitelisting
            .iter()
            .any(|rule| rule.allow.iter().any(|pattern| matches(pattern, action)))
        {
            return Ruling::Allow;
        }
        Ruling::Deny {
            rule: whitelisting
                .first()
                .map(|rule| rule.label())
                .unwrap_or_else(|| "an allow list".to_owned()),
            reason: format!("`{action}` is not in any allow list that covers this agent"),
        }
    }

    /// The capacity of a named quota, if this policy sets one.
    pub fn capacity(&self, quota: &str) -> Option<u64> {
        self.quota.get(quota).copied()
    }

    /// What this policy, on its own, says of a sender's word: `None` for
    /// a sender it does not class (every sender that is not a connector's,
    /// and a connector sender when the section is empty).
    pub fn inbound(&self, sender: &AgentRecord) -> Option<InboundRuling> {
        if !sender.via_connector() {
            return None;
        }
        if let Some(entry) = self.inbound.senders.iter().find(|e| e.covers(sender)) {
            return Some(InboundRuling {
                trust: entry.trust,
                rule: format!("inbound.sender {}", entry.label()),
            });
        }
        self.inbound.connector.map(|trust| InboundRuling {
            trust,
            rule: "inbound.connector".to_owned(),
        })
    }
}

/// Inbound trust for a sender, host and project together: the stricter
/// of the two. A project may hold or refuse what the host accepts, and
/// may not accept what the host holds or refuses — the same asymmetry as
/// [`check`], for the same reason.
pub fn inbound(host: &Policy, project: Option<&Policy>, sender: &AgentRecord) -> InboundRuling {
    let host = host.inbound(sender).map(|ruling| InboundRuling {
        rule: format!("host: {}", ruling.rule),
        ..ruling
    });
    let project = project
        .and_then(|p| p.inbound(sender))
        .map(|ruling| InboundRuling {
            rule: format!("project: {}", ruling.rule),
            ..ruling
        });
    match (host, project) {
        (Some(host), Some(project)) => {
            if project.trust > host.trust {
                project
            } else {
                host
            }
        }
        (Some(only), None) | (None, Some(only)) => only,
        (None, None) => InboundRuling::accepted(),
    }
}

/// The host's policy and, where the agent is in a project that has one,
/// the project's.
///
/// A project may narrow and never widen, so the host is asked first and
/// its refusal is final. There is no order in which a repository can
/// grant itself something the machine's owner did not.
pub fn check(host: &Policy, project: Option<&Policy>, agent: &AgentRecord, action: &str) -> Ruling {
    match host.check(agent, action) {
        Ruling::Deny { rule, reason } => Ruling::Deny {
            rule: format!("host: {rule}"),
            reason,
        },
        Ruling::Allow => match project {
            None => Ruling::Allow,
            Some(project) => match project.check(agent, action) {
                Ruling::Deny { rule, reason } => Ruling::Deny {
                    rule: format!("project: {rule}"),
                    reason,
                },
                Ruling::Allow => Ruling::Allow,
            },
        },
    }
}

/// The tighter of the two capacities. A project may lower a quota the
/// host set, and may set one the host did not; it may not raise one.
pub fn capacity(host: &Policy, project: Option<&Policy>, quota: &str) -> Option<u64> {
    match (
        host.capacity(quota),
        project.and_then(|p| p.capacity(quota)),
    ) {
        (Some(host), Some(project)) => Some(host.min(project)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

/// Glob matching for action patterns.
///
/// `*` matches within one segment and `**` matches across them, the way
/// path globs everywhere else do — `claim:path:/repo/src/*` is the files
/// in a directory and `claim:path:/repo/src/**` is the tree. Segments
/// are separated by `/`; a `:` is an ordinary character, so
/// `send:project:*` reads as it looks.
pub fn matches(pattern: &str, text: &str) -> bool {
    glob(pattern.as_bytes(), text.as_bytes())
}

fn glob(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.first() {
        None => text.is_empty(),
        Some(b'*') => {
            if pattern.get(1) == Some(&b'*') {
                // `**` crosses separators: try every split.
                let rest = &pattern[2..];
                (0..=text.len()).any(|at| glob(rest, &text[at..]))
            } else {
                let rest = &pattern[1..];
                // `*` stops at a separator.
                let limit = text.iter().position(|b| *b == b'/').unwrap_or(text.len());
                (0..=limit).any(|at| glob(rest, &text[at..]))
            }
        }
        Some(b'?') => !text.is_empty() && text[0] != b'/' && glob(&pattern[1..], &text[1..]),
        Some(expected) => {
            !text.is_empty() && text[0] == *expected && glob(&pattern[1..], &text[1..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentSpec, ProjectRef};

    fn agent(name: &str, runtime: &str) -> AgentRecord {
        AgentRecord::new(
            AgentSpec {
                name: name.to_owned(),
                runtime: runtime.to_owned(),
                ..AgentSpec::default()
            },
            true,
            chrono::Utc::now(),
        )
    }

    fn in_project(mut record: AgentRecord, fingerprint: &str, dir: &str) -> AgentRecord {
        let mut project = ProjectRef::directory(dir);
        project.fingerprint = Some(fingerprint.to_owned());
        record.project = Some(project);
        record
    }

    fn rule(toml: &str) -> Policy {
        toml::from_str(toml).expect("a policy")
    }

    #[test]
    fn a_rule_on_a_role_covers_each_agent_that_holds_it_among_others() {
        let policy = rule(
            r#"
[[rule]]
name = "explorers change nothing"
labels = { role = "explorer" }
deny = ["claim:path:/repo/**"]
"#,
        );
        let mut both = agent("a", "codex");
        both.spec
            .labels
            .insert(crate::agent::ROLE_LABEL.into(), "explorer,tester".into());
        assert!(!policy.check(&both, "claim:path:/repo/x").is_allowed());
        let mut near = agent("b", "codex");
        near.spec
            .labels
            .insert(crate::agent::ROLE_LABEL.into(), "explorer-2".into());
        assert!(policy.check(&near, "claim:path:/repo/x").is_allowed());
    }

    #[test]
    fn nothing_written_allows_everything() {
        let policy = Policy::default();
        assert!(
            policy
                .check(&agent("a", "claude-code"), "claim:path:/repo/x")
                .is_allowed()
        );
    }

    #[test]
    fn a_deny_refuses_and_says_which_rule() {
        let policy = rule(
            r#"
[[rule]]
name = "migrations are mine"
deny = ["claim:path:/repo/migrations/**"]
"#,
        );
        let agent = agent("writer", "claude-code");
        let denied = policy.check(&agent, "claim:path:/repo/migrations/001.sql");
        assert!(matches!(&denied, Ruling::Deny { rule, .. } if rule == "migrations are mine"));
        // And leaves everything else alone.
        assert!(
            policy
                .check(&agent, "claim:path:/repo/src/lib.rs")
                .is_allowed()
        );
    }

    #[test]
    fn an_allow_list_turns_on_whitelisting_for_the_agents_it_covers() {
        let policy = rule(
            r#"
[[rule]]
name = "readers"
runtime = "codex"
allow = ["claim:path:/repo/docs/**", "send:**"]
"#,
        );
        let codex = agent("reader", "codex");
        assert!(
            policy
                .check(&codex, "claim:path:/repo/docs/a.md")
                .is_allowed()
        );
        assert!(policy.check(&codex, "send:project:abc").is_allowed());
        // Whitelisting is on for this agent, so anything else is out.
        let denied = policy.check(&codex, "claim:path:/repo/src/lib.rs");
        assert!(
            matches!(&denied, Ruling::Deny { reason, .. } if reason.contains("not in any allow list")),
            "{denied:?}"
        );
        // An agent the rule does not cover is unaffected.
        let claude = agent("writer", "claude-code");
        assert!(
            policy
                .check(&claude, "claim:path:/repo/src/lib.rs")
                .is_allowed()
        );
    }

    #[test]
    fn a_deny_beats_an_allow_however_they_are_ordered() {
        let allow_then_deny = rule(
            r#"
[[rule]]
allow = ["claim:**"]
[[rule]]
deny = ["claim:path:/repo/migrations/**"]
"#,
        );
        let deny_then_allow = rule(
            r#"
[[rule]]
deny = ["claim:path:/repo/migrations/**"]
[[rule]]
allow = ["claim:**"]
"#,
        );
        let agent = agent("a", "claude-code");
        for policy in [allow_then_deny, deny_then_allow] {
            assert!(
                !policy
                    .check(&agent, "claim:path:/repo/migrations/1.sql")
                    .is_allowed(),
                "a deny is not a matter of ordering"
            );
            assert!(
                policy
                    .check(&agent, "claim:path:/repo/src/a.rs")
                    .is_allowed()
            );
        }
    }

    #[test]
    fn rules_are_matched_by_runtime_name_project_and_labels() {
        let policy = rule(
            r#"
[[rule]]
agent = "claude-*"
deny = ["run:**"]
[[rule]]
project = "backend"
deny = ["send:all"]
[[rule]]
labels = { tier = "untrusted" }
deny = ["claim:**"]
"#,
        );
        let named = agent("claude-main", "claude-code");
        assert!(!policy.check(&named, "run:anything").is_allowed());
        assert!(
            policy
                .check(&agent("codex-1", "codex"), "run:anything")
                .is_allowed()
        );

        let backend = in_project(agent("a", "codex"), "abc123", "/repos/backend");
        assert!(!policy.check(&backend, "send:all").is_allowed());
        let other = in_project(agent("a", "codex"), "def456", "/repos/frontend");
        assert!(policy.check(&other, "send:all").is_allowed());

        let mut untrusted = agent("x", "codex");
        untrusted
            .spec
            .labels
            .insert("tier".to_owned(), "untrusted".to_owned());
        assert!(!policy.check(&untrusted, "claim:task:x").is_allowed());
        assert!(
            policy
                .check(&agent("y", "codex"), "claim:task:x")
                .is_allowed()
        );
    }

    #[test]
    fn a_project_may_narrow_the_host_and_never_widen_it() {
        let host = rule(
            r#"
[[rule]]
name = "no migrations"
deny = ["claim:path:/repo/migrations/**"]
"#,
        );
        let permissive_project = rule(
            r#"
[[rule]]
allow = ["claim:**"]
"#,
        );
        let agent = agent("a", "claude-code");
        // The project says everything is fine. The host says otherwise,
        // and the host is the machine owner's file.
        let denied = check(
            &host,
            Some(&permissive_project),
            &agent,
            "claim:path:/repo/migrations/1.sql",
        );
        assert!(
            matches!(&denied, Ruling::Deny { rule, .. } if rule.starts_with("host:")),
            "{denied:?}"
        );

        // Narrowing works: the project forbids something the host allows.
        let strict_project = rule(
            r#"
[[rule]]
name = "release branch is protected"
deny = ["claim:branch:release"]
"#,
        );
        let denied = check(&host, Some(&strict_project), &agent, "claim:branch:release");
        assert!(
            matches!(&denied, Ruling::Deny { rule, .. } if rule.starts_with("project:")),
            "{denied:?}"
        );
        assert!(check(&host, Some(&strict_project), &agent, "claim:branch:main").is_allowed());
    }

    #[test]
    fn a_quota_can_be_lowered_by_a_project_but_not_raised() {
        let host = rule("[quota]\ntokens = 100000\n");
        let lower = rule("[quota]\ntokens = 10000\n");
        let higher = rule("[quota]\ntokens = 999999\n");
        assert_eq!(capacity(&host, Some(&lower), "tokens"), Some(10_000));
        assert_eq!(
            capacity(&host, Some(&higher), "tokens"),
            Some(100_000),
            "a repository cannot vote itself a bigger budget"
        );
        // A project may set one the host did not.
        assert_eq!(
            capacity(&Policy::default(), Some(&lower), "tokens"),
            Some(10_000)
        );
        // And an unmentioned quota is unlimited, so a typo loosens nothing.
        assert_eq!(capacity(&host, Some(&lower), "cpu"), None);
    }

    #[test]
    fn globs_separate_a_directory_from_a_tree() {
        // `*` stays inside a segment; `**` crosses.
        assert!(matches(
            "claim:path:/repo/src/*",
            "claim:path:/repo/src/a.rs"
        ));
        assert!(!matches(
            "claim:path:/repo/src/*",
            "claim:path:/repo/src/deep/a.rs"
        ));
        assert!(matches(
            "claim:path:/repo/src/**",
            "claim:path:/repo/src/deep/a.rs"
        ));
        // `**` also matches nothing at all.
        assert!(matches("claim:**", "claim:"));
        assert!(matches("send:**", "send:project:abc"));
        // A colon is an ordinary character, so this reads as it looks.
        assert!(matches("send:project:*", "send:project:abc123"));
        assert!(!matches("send:project:*", "send:all"));
        // Anchored at both ends: a pattern is not a substring search.
        assert!(!matches("claim:task:x", "claim:task:xy"));
        assert!(!matches("task:x", "claim:task:x"));
        // `?` is one character, and not a separator.
        assert!(matches("claim:task:?", "claim:task:x"));
        assert!(!matches("claim:task:?", "claim:task:xy"));
        assert!(!matches("a?b", "a/b"));
    }

    #[test]
    fn a_pattern_that_is_all_stars_cannot_run_away() {
        // Pathological globs are the classic way a matcher becomes a
        // denial of service. This one is bounded because `**` only ever
        // splits the remaining text.
        assert!(matches("**", "anything:at:all"));
        assert!(matches("*:*:*", "a:b:c"));
        assert!(!matches("**x", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaay"));
    }

    /// A connector sender: no process here, labelled by the connector.
    fn connector(name: &str, runtime: &str) -> AgentRecord {
        let mut record = agent(name, runtime);
        record.managed = false;
        record.pid = None;
        record
            .spec
            .labels
            .insert(crate::agent::CONNECTOR_LABEL.to_owned(), "true".to_owned());
        record
    }

    #[test]
    fn inbound_trust_classes_only_connector_senders() {
        let policy = rule("[inbound]\nconnector = \"refuse\"\n");
        // A session running here is not a remote sender, whatever the
        // section says.
        assert_eq!(policy.inbound(&agent("local", "claude-code")), None);
        let remote = connector("claude-connector-1a2b", "claude-browser");
        let ruling = policy
            .inbound(&remote)
            .expect("a connector sender is classed");
        assert_eq!(ruling.trust, Trust::Refuse);
        assert_eq!(ruling.rule, "inbound.connector");
        // And with nothing written, a connector sender is not classed.
        assert_eq!(Policy::default().inbound(&remote), None);
        assert_eq!(
            inbound(&Policy::default(), None, &remote).trust,
            Trust::Accept
        );
    }

    #[test]
    fn a_sender_entry_decides_before_the_class_and_the_first_match_wins() {
        let policy = rule(
            r#"
[inbound]
connector = "hold"
[[inbound.sender]]
name = "my own account"
agent = "claude-connector-*"
trust = "accept"
[[inbound.sender]]
runtime = "chatgpt-browser"
trust = "refuse"
[[inbound.sender]]
agent = "claude-connector-*"
trust = "refuse"
"#,
        );
        let mine = policy
            .inbound(&connector("claude-connector-7f3a", "claude-browser"))
            .unwrap();
        assert_eq!(
            (mine.trust, mine.rule.as_str()),
            (Trust::Accept, "inbound.sender my own account")
        );
        let chatgpt = policy
            .inbound(&connector("chatgpt-connector-9c", "chatgpt-browser"))
            .unwrap();
        assert_eq!(
            (chatgpt.trust, chatgpt.rule.as_str()),
            (Trust::Refuse, "inbound.sender runtime=chatgpt-browser")
        );
        // Nothing names this one: the class decides.
        let other = policy
            .inbound(&connector("helper", "claude-browser"))
            .unwrap();
        assert_eq!(
            (other.trust, other.rule.as_str()),
            (Trust::Hold, "inbound.connector")
        );
    }

    #[test]
    fn a_project_may_hold_or_refuse_what_the_host_accepts_and_never_the_reverse() {
        let remote = connector("claude-connector-7f3a", "claude-browser");
        let accept = rule("[inbound]\nconnector = \"accept\"\n");
        let hold = rule("[inbound]\nconnector = \"hold\"\n");
        let refuse = rule("[inbound]\nconnector = \"refuse\"\n");
        // The project narrows.
        let ruling = inbound(&accept, Some(&hold), &remote);
        assert_eq!(ruling.trust, Trust::Hold);
        assert!(ruling.rule.starts_with("project:"), "{ruling:?}");
        // The project cannot widen: a repository does not vote itself in.
        let ruling = inbound(&refuse, Some(&accept), &remote);
        assert_eq!(ruling.trust, Trust::Refuse);
        assert!(ruling.rule.starts_with("host:"), "{ruling:?}");
        // Equal classes are the host's word.
        let ruling = inbound(&hold, Some(&hold), &remote);
        assert_eq!(ruling.trust, Trust::Hold);
        assert!(ruling.rule.starts_with("host:"), "{ruling:?}");
        // A project alone sets what the host did not.
        let ruling = inbound(&Policy::default(), Some(&refuse), &remote);
        assert_eq!(ruling.trust, Trust::Refuse);
        assert!(ruling.rule.starts_with("project:"), "{ruling:?}");
        assert!(Trust::Accept < Trust::Hold && Trust::Hold < Trust::Refuse);
    }

    #[test]
    fn an_inbound_entry_with_a_typo_is_refused_rather_than_ignored() {
        let bad: Result<Policy, _> =
            toml::from_str("[[inbound.sender]]\nagnet = \"x\"\ntrust = \"hold\"\n");
        assert!(bad.is_err());
        let bad: Result<Policy, _> = toml::from_str("[inbound]\nconnector = \"maybe\"\n");
        assert!(bad.is_err(), "an unknown class is an error, not accept");
    }

    #[test]
    fn an_unknown_field_is_refused_rather_than_ignored() {
        // A policy is a security boundary: a misspelt `dney = [...]`
        // must not silently permit everything it meant to forbid.
        let bad: Result<Policy, _> = toml::from_str("[[rule]]\ndney = [\"claim:**\"]\n");
        assert!(bad.is_err(), "a typo in a policy file is an error");
    }
}
