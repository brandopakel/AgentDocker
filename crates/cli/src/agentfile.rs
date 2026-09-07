//! `Agentfile.toml`: several agents described together, the way a compose
//! file describes several containers.
//!
//! ```toml
//! name = "backend"                # optional; every agent gets label team=backend
//!
//! [agents.writer]
//! runtime = "claude-code"
//! command = ["claude", "-p", "implement the parser"]
//! workdir = "."                   # relative to this file
//!
//! [agents.reviewer]
//! runtime = "codex"
//! command = ["codex", "exec", "review src/"]
//! env = { RUST_LOG = "info" }
//! labels = { role = "review" }
//! ```
//!
//! Agents start in the order they are written.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use agentdocker_core::AgentSpec;
use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use serde::Deserialize;

/// Looked for in the current directory when no `-f` is given.
pub const DEFAULT_FILES: &[&str] = &["Agentfile.toml", "agentfile.toml"];

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Agentfile {
    /// Team name; every agent gets a `team=<name>` label.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub agents: IndexMap<String, AgentEntry>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct AgentEntry {
    #[serde(default = "default_runtime")]
    pub runtime: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub command: Vec<String>,
    /// Relative paths resolve against the Agentfile's directory.
    #[serde(default)]
    pub workdir: Option<PathBuf>,
    /// Give the agent its own linked worktree and branch when it runs.
    #[serde(default)]
    pub isolate: bool,
    /// Give the agent a terminal rather than pipes, so an interactive
    /// runtime works under `up` and `attach` can reach it.
    #[serde(default)]
    pub tty: bool,
    /// Bring the agent back when `agentd` restarts, under the same
    /// identity and in the same directory.
    #[serde(default)]
    pub restore: bool,
    /// Put the agent in a `tmux` pane instead of running it here, so a
    /// person can reach it with `tmux attach`.
    #[serde(default)]
    pub in_pane: bool,
    /// When to start it again after it exits: `no` (the default),
    /// `always`, `on-failure`, or `on-failure:<n>`.
    #[serde(default)]
    pub restart: Option<String>,
    /// Agents that must be running before this one starts. `up` waits
    /// for each in turn.
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// The first `depends_on` cycle, as the names round it.
///
/// `up` starts agents in order and waits for each dependency, so a cycle
/// is a wait that can never end. Finding it while the file is being read
/// turns a hang into a sentence.
fn dependency_cycle(agents: &IndexMap<String, AgentEntry>) -> Option<Vec<String>> {
    for start in agents.keys() {
        let mut path = vec![start.clone()];
        let mut seen: std::collections::HashSet<&String> = std::collections::HashSet::new();
        seen.insert(start);
        let mut current = start;
        // Follow one dependency at a time; any cycle is reachable from
        // one of its own members, so starting at each name finds them all.
        while let Some(next) = agents
            .get(current)
            .and_then(|entry| entry.depends_on.first())
        {
            path.push(next.clone());
            if next == start {
                return Some(path);
            }
            if !seen.insert(next) {
                break;
            }
            current = next;
        }
    }
    None
}

fn default_runtime() -> String {
    "custom".to_owned()
}

impl AgentEntry {
    /// The restart policy this entry asks for. Unreadable text is `no`;
    /// the file is validated when it is parsed, so by here a bad value
    /// has already been reported with the name of the agent it is on.
    pub fn restart_policy(&self) -> agentdocker_core::RestartPolicy {
        self.restart
            .as_deref()
            .and_then(agentdocker_core::RestartPolicy::parse)
            .unwrap_or_default()
    }
}

impl Agentfile {
    pub fn parse(text: &str) -> Result<Self> {
        let file: Self = toml::from_str(text)?;
        for (name, entry) in &file.agents {
            if name.is_empty() {
                bail!("agent names must not be empty");
            }
            if entry.command.first().is_none_or(String::is_empty) {
                bail!("agent `{name}` has an empty command");
            }
            if let Some(restart) = &entry.restart
                && agentdocker_core::RestartPolicy::parse(restart).is_none()
            {
                bail!(
                    "agent `{name}` has restart = \"{restart}\"; use no, always, on-failure, \
                     or on-failure:<n>"
                );
            }
            // A dependency that is not in the file can never start, so
            // `up` would wait for it forever. Say so while the file is
            // being read, with both names.
            for needed in &entry.depends_on {
                if needed == name {
                    bail!("agent `{name}` depends on itself");
                }
                if !file.agents.contains_key(needed) {
                    bail!("agent `{name}` depends on `{needed}`, which is not in this file");
                }
            }
        }
        if let Some(cycle) = dependency_cycle(&file.agents) {
            bail!(
                "agents depend on each other in a cycle: {}",
                cycle.join(" → ")
            );
        }
        Ok(file)
    }

    /// Read `path`, or the first default file name in the current directory.
    /// Returns the file and its canonical path.
    pub fn load(path: Option<&Path>) -> Result<(Self, PathBuf)> {
        let path = match path {
            Some(path) => path.to_path_buf(),
            None => DEFAULT_FILES
                .iter()
                .map(PathBuf::from)
                .find(|candidate| candidate.exists())
                .with_context(|| {
                    format!(
                        "no {} in the current directory (use -f)",
                        DEFAULT_FILES.join(" or ")
                    )
                })?,
        };
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let file = Self::parse(&text)
            .with_context(|| format!("{} is not a valid Agentfile", path.display()))?;
        let path = path.canonicalize().unwrap_or(path);
        Ok((file, path))
    }

    /// Specs for the named agents (all when `only` is empty), in file
    /// order, with working directories resolved and bookkeeping labels
    /// (`agentfile`, `team`) added.
    pub fn specs(&self, file_path: &Path, only: &[String]) -> Result<Vec<AgentSpec>> {
        for name in only {
            if !self.agents.contains_key(name) {
                bail!("no agent `{name}` in {}", file_path.display());
            }
        }
        let base = file_path.parent().unwrap_or(Path::new("."));
        let specs = self
            .agents
            .iter()
            .filter(|(name, _)| only.is_empty() || only.iter().any(|wanted| wanted == *name))
            .map(|(name, entry)| {
                let workdir = entry.workdir.clone().unwrap_or_else(|| PathBuf::from("."));
                let workdir = if workdir.is_absolute() {
                    workdir
                } else {
                    base.join(workdir)
                };
                let mut labels = entry.labels.clone();
                labels.insert("agentfile".to_owned(), file_path.display().to_string());
                if let Some(team) = &self.name {
                    labels.insert("team".to_owned(), team.clone());
                }
                AgentSpec {
                    name: name.clone(),
                    runtime: entry.runtime.clone(),
                    provider: entry.provider.clone(),
                    model: entry.model.clone(),
                    command: entry.command.clone(),
                    workdir: Some(workdir.canonicalize().unwrap_or(workdir)),
                    env: entry.env.clone(),
                    labels,
                    isolate: entry.isolate,
                    tty: entry.tty,
                    restore: entry.restore,
                    in_pane: entry.in_pane,
                    restart: entry.restart_policy(),
                    depends_on: entry.depends_on.clone(),
                }
            })
            .collect();
        Ok(specs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
name = "backend"

[agents.writer]
runtime = "claude-code"
model = "claude-opus-5"
command = ["claude", "-p", "implement"]
workdir = "src"

[agents.reviewer]
command = ["codex", "exec", "review"]
env = { RUST_LOG = "info" }
labels = { role = "review" }
"#;

    #[test]
    fn parses_in_file_order_with_defaults() {
        let file = Agentfile::parse(SAMPLE).unwrap();
        assert_eq!(file.name.as_deref(), Some("backend"));
        let names: Vec<&String> = file.agents.keys().collect();
        assert_eq!(names, ["writer", "reviewer"]);
        assert_eq!(file.agents["reviewer"].runtime, "custom");
        assert_eq!(
            file.agents["writer"].model.as_deref(),
            Some("claude-opus-5")
        );
    }

    #[test]
    fn specs_resolve_workdir_and_add_labels() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let file_path = dir.path().join("Agentfile.toml");
        let file = Agentfile::parse(SAMPLE).unwrap();
        let specs = file.specs(&file_path, &[]).unwrap();
        assert_eq!(specs.len(), 2);

        let writer = &specs[0];
        assert_eq!(writer.name, "writer");
        assert_eq!(
            writer.workdir.as_deref(),
            Some(dir.path().join("src").canonicalize().unwrap().as_path())
        );
        assert_eq!(writer.labels["team"], "backend");
        assert_eq!(writer.labels["agentfile"], file_path.display().to_string());

        let reviewer = &specs[1];
        assert_eq!(
            reviewer.workdir.as_deref(),
            Some(dir.path().canonicalize().unwrap().as_path())
        );
        assert_eq!(reviewer.labels["role"], "review");
        assert_eq!(reviewer.env["RUST_LOG"], "info");
    }

    #[test]
    fn only_filters_and_rejects_unknown_names() {
        let file = Agentfile::parse(SAMPLE).unwrap();
        let path = Path::new("/x/Agentfile.toml");
        let specs = file.specs(path, &["reviewer".to_owned()]).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "reviewer");
        assert!(file.specs(path, &["nope".to_owned()]).is_err());
    }

    #[test]
    fn rejects_unknown_fields_and_empty_commands() {
        assert!(Agentfile::parse("[agents.a]\ncommand = []\n").is_err());
        assert!(Agentfile::parse("[agents.a]\ncommand = [\"\"]\n").is_err());
        assert!(Agentfile::parse("[agents.a]\ncommand = [\"x\"]\nbogus = 1\n").is_err());
        assert!(Agentfile::parse("[agents.a]\nruntime = \"x\"\n").is_err());
        assert!(Agentfile::parse("").unwrap().agents.is_empty());
    }

    #[test]
    fn dependencies_start_before_the_agents_that_need_them() {
        let file = Agentfile::parse(
            r#"
[agents.web]
command = ["sh", "-c", "serve"]
depends_on = ["db", "cache"]

[agents.cache]
command = ["sh", "-c", "cache"]
depends_on = ["db"]

[agents.db]
command = ["sh", "-c", "db"]
"#,
        )
        .unwrap();
        let specs = file.specs(Path::new("/repo"), &[]).unwrap();
        let ordered: Vec<String> = crate::teams::order(specs)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(ordered, ["db", "cache", "web"]);
    }

    #[test]
    fn file_order_survives_where_dependencies_do_not_decide() {
        let file = Agentfile::parse(
            r#"
[agents.zeta]
command = ["sh", "-c", "z"]

[agents.alpha]
command = ["sh", "-c", "a"]
"#,
        )
        .unwrap();
        let specs = file.specs(Path::new("/repo"), &[]).unwrap();
        let ordered: Vec<String> = crate::teams::order(specs)
            .into_iter()
            .map(|s| s.name)
            .collect();
        // Not sorted: the order somebody wrote them in is information.
        assert_eq!(ordered, ["zeta", "alpha"]);
    }

    #[test]
    fn a_dependency_that_cannot_be_satisfied_is_refused_when_the_file_is_read() {
        // `up` starts agents in order and waits for each dependency, so
        // every one of these would otherwise be a wait that never ends.
        let missing = Agentfile::parse(
            r#"
[agents.web]
command = ["sh", "-c", "serve"]
depends_on = ["nowhere"]
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(missing.contains("nowhere"), "{missing}");
        assert!(missing.contains("not in this file"), "{missing}");

        let itself = Agentfile::parse(
            r#"
[agents.web]
command = ["sh", "-c", "serve"]
depends_on = ["web"]
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(itself.contains("depends on itself"), "{itself}");

        let ring = Agentfile::parse(
            r#"
[agents.a]
command = ["sh", "-c", "a"]
depends_on = ["b"]

[agents.b]
command = ["sh", "-c", "b"]
depends_on = ["a"]
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(ring.contains("cycle"), "{ring}");
    }

    #[test]
    fn a_restart_policy_is_read_from_the_file_and_a_bad_one_is_named() {
        let file = Agentfile::parse(
            r#"
[agents.web]
command = ["sh", "-c", "serve"]
restart = "on-failure:5"

[agents.worker]
command = ["sh", "-c", "work"]
"#,
        )
        .unwrap();
        let specs = file.specs(Path::new("/repo"), &[]).unwrap();
        let web = specs.iter().find(|s| s.name == "web").unwrap();
        assert_eq!(
            web.restart,
            agentdocker_core::RestartPolicy::OnFailure { max: 5 }
        );
        let worker = specs.iter().find(|s| s.name == "worker").unwrap();
        assert!(worker.restart.is_no(), "the default is not to restart");

        let bad = Agentfile::parse(
            r#"
[agents.web]
command = ["sh", "-c", "serve"]
restart = "sometimes"
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(bad.contains("web"), "the agent is named: {bad}");
        assert!(bad.contains("sometimes"), "and so is the value: {bad}");
    }
}
