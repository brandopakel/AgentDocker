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
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

fn default_runtime() -> String {
    "custom".to_owned()
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
}
