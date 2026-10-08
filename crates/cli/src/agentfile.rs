//! `Agentfile.toml`: several agents described together, the way a compose
//! file describes several containers.
//!
//! ```toml
//! #:schema https://raw.githubusercontent.com/brandopakel/AgentDocker/main/crates/cli/schemas/agentfile.schema.json
//! version = 2
//! name = "backend"                # optional; every agent gets label team=backend
//!
//! [agents.writer]
//! runtime = "claude-code"
//! prompt = "Implement the parser in src/parser.rs"
//! model = "${WRITER_MODEL:-opus}"
//! effort = "high"
//! workdir = "."                   # relative to this file
//!
//! [agents.reviewer]
//! runtime = "codex"
//! command = ["codex", "exec", "review src/"]
//! env = { RUST_LOG = "info" }
//! labels = { role = "review" }
//! ```
//!
//! Agents start in the order they are written, dependencies first.
//!
//! A file without `version` is version 1, the format before versions: it is
//! read as it was written and upgraded in memory ([`v1`]). Each field below
//! says the version that introduced it, so a field a file's version does not
//! have is refused with the version to ask for rather than as unknown.

mod interpolate;
pub mod launch;
mod v1;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use agentdocker_core::AgentSpec;
use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// Looked for in the current directory when no `-f` is given.
pub const DEFAULT_FILES: &[&str] = &["Agentfile.toml", "agentfile.toml"];

/// The newest version this build reads and writes.
pub const LATEST: u32 = 2;

/// The JSON Schema for the latest version, for editors (`#:schema` in a
/// TOML file) and for `agentdocker agentfile schema`.
pub const SCHEMA: &str = include_str!("../schemas/agentfile.schema.json");

/// Where the schema is published: the file above, on the default branch.
pub const SCHEMA_URL: &str = "https://raw.githubusercontent.com/brandopakel/AgentDocker/main/crates/cli/schemas/agentfile.schema.json";

/// Top-level fields, each with the version that introduced it. `version`
/// itself is accepted in every version, so a file can pin itself to 1.
const TOP_FIELDS: &[(&str, u32)] = &[("version", 1), ("name", 1), ("agents", 1)];

/// An agent's fields, each with the version that introduced it.
pub const AGENT_FIELDS: &[(&str, u32)] = &[
    ("runtime", 1),
    ("provider", 1),
    ("model", 1),
    ("command", 1),
    ("workdir", 1),
    ("isolate", 1),
    ("tty", 1),
    ("restore", 1),
    ("in_pane", 1),
    ("restart", 1),
    ("depends_on", 1),
    ("env", 1),
    ("labels", 1),
    ("prompt", 2),
    ("effort", 2),
    ("args", 2),
    ("config", 2),
    ("idle_messages", 2),
    ("coordinate", 2),
];

/// The latest version, in memory. Older files are upgraded into this.
#[derive(Deserialize, Serialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Agentfile {
    #[serde(default = "latest")]
    pub version: u32,
    /// Team name; every agent gets a `team=<name>` label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub agents: IndexMap<String, AgentEntry>,
}

fn latest() -> u32 {
    LATEST
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct AgentEntry {
    #[serde(default = "default_runtime")]
    pub runtime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The model. On a command agentdocker builds it is passed to the
    /// runtime; on a written `command` it is a label for `ps`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The command line. Optional from version 2 for a runtime agentdocker
    /// can build a command for ([`launch::BUILT`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// Relative paths resolve against the Agentfile's directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    /// Give the agent its own linked worktree and branch when it runs.
    #[serde(default, skip_serializing_if = "is_false")]
    pub isolate: bool,
    /// Give the agent a terminal rather than pipes, so an interactive
    /// runtime works under `up` and `attach` can reach it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub tty: bool,
    /// Bring the agent back when `agentd` restarts, under the same
    /// identity and in the same directory.
    #[serde(default, skip_serializing_if = "is_false")]
    pub restore: bool,
    /// Put the agent in a `tmux` pane instead of running it here, so a
    /// person can reach it with `tmux attach`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub in_pane: bool,
    /// When to start it again after it exits: `no` (the default),
    /// `always`, `on-failure`, or `on-failure:<n>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<String>,
    /// Agents that must be running before this one starts. `up` waits
    /// for each in turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Version 2: the first instruction, for a command agentdocker builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Version 2: reasoning effort, in the runtime's own words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Version 2: provider arguments added to a built command, before the
    /// prompt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Version 2: docker-agent's agent YAML file, or its registry reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
    /// Version 2: let a message start a turn while the session is idle
    /// (Claude Code's channel, Codex's input adapter; both experimental).
    #[serde(default, skip_serializing_if = "is_false")]
    pub idle_messages: bool,
    /// Version 2: wire AgentDocker into a built command (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinate: Option<bool>,
}

fn is_false(value: &bool) -> bool {
    !value
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

pub(crate) fn default_runtime() -> String {
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

    /// The entry with `${VAR}` expanded in every value that takes it, read
    /// through `lookup`. Names, the runtime, the restart policy and the
    /// dependencies are taken as written.
    fn expanded(&self, name: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<Self> {
        let field = |key: &str| format!("agents.{name}.{key}");
        let one = |key: &str, value: &Option<String>| -> Result<Option<String>> {
            value
                .as_deref()
                .map(|text| interpolate::expand(text, &field(key), lookup))
                .transpose()
                .map_err(anyhow::Error::msg)
        };
        let list = |key: &str, values: &[String]| -> Result<Vec<String>> {
            values
                .iter()
                .enumerate()
                .map(|(i, text)| interpolate::expand(text, &format!("{}[{i}]", field(key)), lookup))
                .collect::<Result<_, _>>()
                .map_err(anyhow::Error::msg)
        };
        let map =
            |key: &str, values: &BTreeMap<String, String>| -> Result<BTreeMap<String, String>> {
                values
                    .iter()
                    .map(|(k, text)| {
                        interpolate::expand(text, &format!("{}.{k}", field(key)), lookup)
                            .map(|value| (k.clone(), value))
                    })
                    .collect::<Result<_, _>>()
                    .map_err(anyhow::Error::msg)
            };
        Ok(Self {
            provider: one("provider", &self.provider)?,
            model: one("model", &self.model)?,
            command: list("command", &self.command)?,
            workdir: one("workdir", &self.workdir)?,
            env: map("env", &self.env)?,
            labels: map("labels", &self.labels)?,
            prompt: one("prompt", &self.prompt)?,
            effort: one("effort", &self.effort)?,
            args: list("args", &self.args)?,
            config: one("config", &self.config)?,
            ..self.clone()
        })
    }
}

/// The version a file declares, refusing one this build cannot read.
fn declared_version(raw: &toml::Table) -> Result<u32> {
    match raw.get("version") {
        None => Ok(1),
        Some(toml::Value::Integer(n)) if (1..=i64::from(LATEST)).contains(n) => Ok(*n as u32),
        Some(toml::Value::Integer(n)) if *n > i64::from(LATEST) => bail!(
            "this Agentfile is version {n}; this agentdocker reads versions 1 to {LATEST}, so update agentdocker"
        ),
        Some(other) => bail!("`version` is a whole number from 1 to {LATEST}, not {other}"),
    }
}

/// Refuse a field the file's version does not have, saying which version
/// introduced it, and a field no version has, saying which ones exist.
fn check_fields(raw: &toml::Table, version: u32) -> Result<()> {
    let judge = |key: &str, table: &[(&str, u32)], place: &str| -> Result<()> {
        match table.iter().find(|(field, _)| *field == key) {
            Some((_, since)) if *since > version => bail!(
                "`{key}`{place} arrived in Agentfile version {since}; this file is version {version}, so add `version = {since}` at the top{}",
                if version == 1 {
                    " (from version 2, `$` in a value starts `${VAR}`: write `$$` for a dollar sign, or let `agentdocker agentfile upgrade` do it)"
                } else {
                    ""
                }
            ),
            Some(_) => Ok(()),
            None => bail!(
                "unknown field `{key}`{place}; version {version} has {}",
                table
                    .iter()
                    .filter(|(_, since)| *since <= version)
                    .map(|(field, _)| *field)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    };
    for key in raw.keys() {
        judge(key, TOP_FIELDS, "")?;
    }
    if let Some(toml::Value::Table(agents)) = raw.get("agents") {
        for (name, entry) in agents {
            if let toml::Value::Table(entry) = entry {
                for key in entry.keys() {
                    judge(key, AGENT_FIELDS, &format!(" in agent `{name}`"))?;
                }
            }
        }
    }
    Ok(())
}

impl Agentfile {
    /// Read a file of any version this build knows, upgraded to the latest,
    /// and checked for everything that can be judged without the
    /// environment: `${VAR}` is expanded later, by [`Self::specs`].
    pub fn parse(text: &str) -> Result<Self> {
        let raw: toml::Table = toml::from_str(text)?;
        let version = declared_version(&raw)?;
        check_fields(&raw, version)?;
        let file = match version {
            1 => toml::Value::Table(raw)
                .try_into::<v1::Agentfile>()?
                .upgrade(),
            _ => toml::Value::Table(raw).try_into::<Agentfile>()?,
        };
        for (name, entry) in &file.agents {
            if name.is_empty() {
                bail!("agent names must not be empty");
            }
            if !entry.command.is_empty() && entry.command[0].is_empty() {
                bail!("agent `{name}` has an empty command");
            }
            if version == 1 && entry.command.is_empty() {
                bail!("agent `{name}` has an empty command");
            }
            launch::check(name, entry).map_err(anyhow::Error::msg)?;
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

    /// The file as the latest version would write it, for `agentfile
    /// upgrade`. Comments are not kept: TOML has no place for them in the
    /// data, so the person merges what they want back.
    pub fn render(&self) -> Result<String> {
        Ok(format!(
            "#:schema {SCHEMA_URL}\n{}",
            toml::to_string(self).context("cannot write the upgraded file")?
        ))
    }

    /// The names `only` selects (all when empty), in file order, refusing a
    /// name the file does not have.
    pub fn names(&self, file_path: &Path, only: &[String]) -> Result<Vec<String>> {
        for name in only {
            if !self.agents.contains_key(name) {
                bail!("no agent `{name}` in {}", file_path.display());
            }
        }
        Ok(self
            .agents
            .keys()
            .filter(|name| only.is_empty() || only.contains(name))
            .cloned()
            .collect())
    }

    /// Specs for the named agents (all when `only` is empty), in file
    /// order, with `${VAR}` expanded, working directories resolved, built
    /// commands built and bookkeeping labels (`agentfile`, `team`) added;
    /// with each, the notes building it left for the person.
    pub fn specs(
        &self,
        file_path: &Path,
        only: &[String],
        host: &dyn launch::Host,
    ) -> Result<Vec<(AgentSpec, Vec<String>)>> {
        let base = file_path.parent().unwrap_or(Path::new("."));
        let lookup = |name: &str| host.var(name);
        self.names(file_path, only)?
            .into_iter()
            .map(|name| {
                let entry = self.agents[&name].expanded(&name, &lookup)?;
                let workdir = PathBuf::from(entry.workdir.clone().unwrap_or_else(|| ".".into()));
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
                let mut spec = AgentSpec {
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
                };
                let notes = launch::apply(&mut spec, &entry, base, host)
                    .with_context(|| format!("agent `{name}`"))?;
                Ok((spec, notes))
            })
            .collect()
    }
}

/// The machine as it is: the process environment, `PATH` and the standard
/// installation directories, and the person's own runtime configuration.
pub struct SystemHost {
    roots: agentdocker_host::runtimes::Roots,
}

impl SystemHost {
    pub fn new() -> Self {
        let mut roots = agentdocker_host::runtimes::Roots::from_env();
        roots.versions = false;
        Self { roots }
    }
}

impl launch::Host for SystemHost {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn program(&self, runtime: &str) -> Option<PathBuf> {
        agentdocker_core::runtime::spec(runtime)
            .and_then(|spec| agentdocker_host::runtimes::find_cli(spec, &self.roots))
    }

    fn agentdocker(&self) -> Result<PathBuf> {
        crate::desktop::setup_executable().context("cannot locate the agentdocker binary")
    }

    fn mcp_wired(&self, runtime: &str) -> bool {
        agentdocker_core::runtime::spec(runtime).is_some_and(|spec| {
            agentdocker_host::runtimes::mcp_wiring(spec, &self.roots, "agentdocker")
                == agentdocker_core::Wiring::Wired
        })
    }

    fn claude_hooks_present(&self, workdir: &Path) -> bool {
        let Some(spec) = agentdocker_core::runtime::spec("claude-code") else {
            return false;
        };
        [
            agentdocker_host::runtimes::hook_config_path(spec, &self.roots),
            workdir.join(".claude/settings.json"),
            workdir.join(".claude/settings.local.json"),
        ]
        .iter()
        .any(|file| agentdocker_host::runtimes::claude_settings_run_adapter(file, "agentdocker"))
    }
}

#[cfg(test)]
mod tests {
    use super::launch::tests::Fake;
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

    fn specs(file: &Agentfile, path: &Path, only: &[String]) -> Vec<AgentSpec> {
        file.specs(path, only, &Fake::default())
            .unwrap()
            .into_iter()
            .map(|(spec, _)| spec)
            .collect()
    }

    #[test]
    fn parses_in_file_order_with_defaults() {
        let file = Agentfile::parse(SAMPLE).unwrap();
        assert_eq!(file.version, LATEST, "upgraded in memory");
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
        let specs = specs(&file, &file_path, &[]);
        assert_eq!(specs.len(), 2);

        let writer = &specs[0];
        assert_eq!(writer.name, "writer");
        assert_eq!(
            writer.workdir.as_deref(),
            Some(dir.path().join("src").canonicalize().unwrap().as_path())
        );
        assert_eq!(writer.labels["team"], "backend");
        assert_eq!(writer.labels["agentfile"], file_path.display().to_string());
        // A written command is launched as written, whatever the runtime.
        assert_eq!(writer.command, ["claude", "-p", "implement"]);

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
        let specs = specs(&file, path, &["reviewer".to_owned()]);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "reviewer");
        assert!(
            file.specs(path, &["nope".to_owned()], &Fake::default())
                .is_err()
        );
        assert_eq!(file.names(path, &[]).unwrap(), ["writer", "reviewer"]);
    }

    #[test]
    fn rejects_unknown_fields_and_empty_commands() {
        assert!(Agentfile::parse("[agents.a]\ncommand = []\n").is_err());
        assert!(Agentfile::parse("[agents.a]\ncommand = [\"\"]\n").is_err());
        let unknown = Agentfile::parse("[agents.a]\ncommand = [\"x\"]\nbogus = 1\n")
            .unwrap_err()
            .to_string();
        assert!(
            unknown.contains("unknown field `bogus` in agent `a`"),
            "{unknown}"
        );
        assert!(unknown.contains("runtime"), "lists what exists: {unknown}");
        assert!(
            !unknown.contains("prompt"),
            "only this version's fields: {unknown}"
        );
        assert!(Agentfile::parse("[agents.a]\nruntime = \"x\"\n").is_err());
        assert!(Agentfile::parse("nonsense = 1\n").is_err());
        assert!(Agentfile::parse("").unwrap().agents.is_empty());
        assert!(
            Agentfile::parse("version = 2\n[agents.a]\nruntime = \"aider\"\n")
                .unwrap_err()
                .to_string()
                .contains("no `command`")
        );
    }

    /// A field from a later version is refused with the version that has
    /// it; a version this build does not know is refused with what to do.
    #[test]
    fn versions_are_declared_and_fields_name_the_version_that_has_them() {
        let early = Agentfile::parse("[agents.w]\nruntime = \"claude-code\"\nprompt = \"go\"\n")
            .unwrap_err()
            .to_string();
        assert!(
            early.contains("`prompt` in agent `w` arrived in Agentfile version 2"),
            "{early}"
        );
        assert!(early.contains("add `version = 2`"), "{early}");
        assert!(
            early.contains("$$"),
            "warns what version 2 changes: {early}"
        );
        let future = Agentfile::parse("version = 9\n").unwrap_err().to_string();
        assert!(
            future.contains("version 9") && future.contains("update agentdocker"),
            "{future}"
        );
        assert!(Agentfile::parse("version = 0\n").is_err());
        assert!(Agentfile::parse("version = \"2\"\n").is_err());
        assert_eq!(Agentfile::parse("version = 1\n").unwrap().version, LATEST);
        assert_eq!(Agentfile::parse("version = 2\n").unwrap().version, 2);
    }

    /// Version 1 never expanded `$`, so upgrading doubles it and the agent
    /// gets exactly the text it always got; version 2 expands.
    #[test]
    fn a_version_one_dollar_stays_a_dollar_and_version_two_expands() {
        let old = r#"
[agents.a]
command = ["sh", "-c", "echo $HOME ${MODEL}"]
env = { PRICE = "$5" }
"#;
        let file = Agentfile::parse(old).unwrap();
        let spec = &specs(&file, Path::new("/x/Agentfile.toml"), &[])[0];
        assert_eq!(spec.command[2], "echo $HOME ${MODEL}");
        assert_eq!(spec.env["PRICE"], "$5");
        let rendered = file.render().unwrap();
        assert!(rendered.starts_with("#:schema "), "{rendered}");
        assert!(rendered.contains("version = 2"), "{rendered}");
        let again = Agentfile::parse(&rendered).unwrap();
        let spec = &specs(&again, Path::new("/x/Agentfile.toml"), &[])[0];
        assert_eq!(
            spec.command[2], "echo $HOME ${MODEL}",
            "the upgrade means the same"
        );

        let new = r#"
version = 2
[agents.a]
command = ["sh", "-c", "echo $$HOME ${MODEL} ${MISSING:-none}"]
env = { WHO = "${TASK}" }
labels = { model = "${MODEL}" }
"#;
        let file = Agentfile::parse(new).unwrap();
        let spec = &specs(&file, Path::new("/x/Agentfile.toml"), &[])[0];
        assert_eq!(spec.command[2], "echo $HOME opus none");
        assert_eq!(spec.env["WHO"], "the parser");
        assert_eq!(spec.labels["model"], "opus");
        let unset = Agentfile::parse("version = 2\n[agents.a]\ncommand = [\"${NOPE}\"]\n")
            .unwrap()
            .specs(Path::new("/x/A.toml"), &[], &Fake::default())
            .unwrap_err();
        let unset = format!("{unset:#}");
        assert!(
            unset.contains("agents.a.command[0]") && unset.contains("NOPE"),
            "{unset}"
        );
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
        let ordered: Vec<String> = crate::teams::order(specs(&file, Path::new("/repo"), &[]))
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
        let ordered: Vec<String> = crate::teams::order(specs(&file, Path::new("/repo"), &[]))
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
        let specs = specs(&file, Path::new("/repo"), &[]);
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

    fn built(text: &str, host: &Fake) -> (AgentSpec, Vec<String>) {
        let file = Agentfile::parse(text).unwrap();
        file.specs(Path::new("/repo/Agentfile.toml"), &[], host)
            .unwrap()
            .remove(0)
    }

    /// A Claude Code entry that says what it wants gets the flags, and the
    /// coordination the person's own configuration lacks; nothing doubled
    /// when it already has it.
    #[test]
    fn claude_code_commands_are_built_with_coordination_wired_in() {
        let text = r#"
version = 2
[agents.writer]
runtime = "claude-code"
prompt = "Implement ${TASK}"
model = "${MODEL}"
effort = "high"
args = ["--permission-mode", "acceptEdits"]
"#;
        let (spec, notes) = built(text, &Fake::default());
        assert!(notes.is_empty());
        let c = &spec.command;
        assert_eq!(
            &c[..6],
            [
                "/bin/claude-code",
                "-p",
                "--model",
                "opus",
                "--effort",
                "high"
            ]
        );
        let mcp = c
            .iter()
            .position(|w| w == "--mcp-config")
            .expect("MCP wired");
        assert!(
            c[mcp + 1].contains(r#""args":["mcp","--runtime","claude-code"]"#),
            "{}",
            c[mcp + 1]
        );
        assert!(c[mcp + 1].contains("/opt/ad/agentdocker"));
        let settings = c
            .iter()
            .position(|w| w == "--settings")
            .expect("hooks wired");
        assert!(
            c[settings + 1].contains("PreToolUse"),
            "{}",
            c[settings + 1]
        );
        assert!(c[settings + 1].contains("/opt/ad/agentdocker hook claude-code"));
        assert_eq!(
            &c[c.len() - 4..],
            [
                "--permission-mode",
                "acceptEdits",
                "--",
                "Implement the parser"
            ]
        );
        assert_eq!(spec.model.as_deref(), Some("opus"));

        let wired = Fake {
            wired: true,
            ..Fake::default()
        };
        let (spec, _) = built(text, &wired);
        assert!(
            !spec
                .command
                .iter()
                .any(|w| w == "--mcp-config" || w == "--settings")
        );

        let (spec, _) = built(
            "version = 2\n[agents.w]\nruntime = \"claude-code\"\nprompt = \"go\"\ncoordinate = false\n",
            &Fake::default(),
        );
        assert_eq!(spec.command, ["/bin/claude-code", "-p", "--", "go"]);
    }

    #[test]
    fn interactive_and_idle_claude_sessions_use_the_channel() {
        let exe = tempfile::NamedTempFile::new().unwrap();
        let host = Fake {
            exe: Some(exe.path().to_owned()),
            ..Fake::default()
        };
        let (spec, _) = built(
            "version = 2\n[agents.w]\nruntime = \"claude-code\"\ntty = true\nidle_messages = true\n",
            &host,
        );
        let c = &spec.command;
        assert_eq!(c[0], "/bin/claude-code");
        assert!(!c.contains(&"-p".to_owned()), "interactive: {c:?}");
        assert!(
            c.contains(&"--dangerously-load-development-channels".to_owned()),
            "{c:?}"
        );
        assert_eq!(
            c.iter().filter(|w| *w == "--mcp-config").count(),
            1,
            "the channel's MCP entry only: {c:?}"
        );
        assert_eq!(
            spec.env[agentdocker_host::provider_input::CLAUDE_CHANNEL_ENV],
            "1"
        );
    }

    #[test]
    fn codex_docker_agent_and_gemini_commands_are_built() {
        let (spec, _) = built(
            "version = 2\n[agents.r]\nruntime = \"codex\"\nprompt = \"review\"\nmodel = \"gpt-5\"\neffort = \"low\"\n",
            &Fake::default(),
        );
        assert_eq!(
            spec.command,
            [
                "/bin/codex",
                "exec",
                "-c",
                "model=\"gpt-5\"",
                "-c",
                "model_reasoning_effort=\"low\"",
                "-c",
                "mcp_servers.agentdocker.command=\"/opt/ad/agentdocker\"",
                "-c",
                r#"mcp_servers.agentdocker.args=["mcp","--runtime","codex"]"#,
                "--",
                "review",
            ]
        );

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("team.yaml"),
            "agents:\n  root:\n    model: x\n",
        )
        .unwrap();
        let file = Agentfile::parse(
            "version = 2\n[agents.d]\nruntime = \"docker-agent\"\nconfig = \"team.yaml\"\nprompt = \"go\"\nmodel = \"anthropic/claude-sonnet-4-5\"\n",
        )
        .unwrap();
        let (spec, notes) = file
            .specs(&dir.path().join("Agentfile.toml"), &[], &Fake::default())
            .unwrap()
            .remove(0);
        let yaml = dir.path().join("team.yaml").canonicalize().unwrap();
        assert_eq!(
            spec.command,
            [
                "/bin/docker-agent",
                "run",
                yaml.to_str().unwrap(),
                "--model",
                "anthropic/claude-sonnet-4-5",
                "--",
                "go",
            ]
        );
        assert!(
            notes[0].contains("no `agentdocker mcp` toolset"),
            "{notes:?}"
        );
        std::fs::write(
            dir.path().join("team.yaml"),
            agentdocker_core::runtime::docker_agent_toolset("/opt/ad/agentdocker"),
        )
        .unwrap();
        let (_, notes) = file
            .specs(&dir.path().join("Agentfile.toml"), &[], &Fake::default())
            .unwrap()
            .remove(0);
        assert!(notes.is_empty(), "{notes:?}");
        // A registry reference is passed as written.
        let (spec, _) = built(
            "version = 2\n[agents.d]\nruntime = \"docker-agent\"\nconfig = \"myorg/agent:1\"\ntty = true\n",
            &Fake::default(),
        );
        assert_eq!(spec.command, ["/bin/docker-agent", "run", "myorg/agent:1"]);

        let (spec, notes) = built(
            "version = 2\n[agents.g]\nruntime = \"gemini-cli\"\nprompt = \"-x\"\n",
            &Fake::default(),
        );
        assert_eq!(spec.command, ["/bin/gemini-cli", "--prompt=-x"]);
        assert!(notes[0].contains("setup gemini-cli"), "{notes:?}");
    }

    /// What a built command cannot be is refused when the file is read,
    /// with the agent's name and what to do.
    #[test]
    fn launch_fields_that_cannot_work_are_refused_with_the_fix() {
        for (text, expect) in [
            (
                "[agents.a]\nruntime = \"claude-code\"\ncommand = [\"claude\"]\nprompt = \"x\"",
                "sets both `command` and `prompt`",
            ),
            (
                "[agents.a]\nruntime = \"claude-code\"\neffort = \"huge\"\nprompt = \"x\"",
                "low, medium, high, xhigh, max",
            ),
            ("[agents.a]\nruntime = \"claude-code\"", "needs a `prompt`"),
            (
                "[agents.a]\nruntime = \"claude-code\"\nprompt = \"x\"\nidle_messages = true",
                "set `tty = true`",
            ),
            (
                "[agents.a]\nruntime = \"codex\"\ntty = true\nidle_messages = true\nprompt = \"x\"",
                "agentdocker send --to a",
            ),
            (
                "[agents.a]\nruntime = \"docker-agent\"\nprompt = \"x\"",
                "needs `config`",
            ),
            (
                "[agents.a]\nruntime = \"docker-agent\"\nconfig = \"a.yaml\"\nprompt = \"x\"\neffort = \"high\"",
                "in the agent's YAML",
            ),
            (
                "[agents.a]\nruntime = \"aider\"\nprompt = \"x\"",
                "cannot build a command for runtime `aider`",
            ),
            (
                "[agents.a]\nruntime = \"codex\"\nconfig = \"a.yaml\"\nprompt = \"x\"",
                "`config` names a docker-agent",
            ),
            (
                "[agents.a]\nruntime = \"gemini-cli\"\ncommand = [\"gemini\"]\nidle_messages = true",
                "claude-code and codex",
            ),
        ] {
            let error = Agentfile::parse(&format!("version = 2\n{text}\n"))
                .unwrap_err()
                .to_string();
            assert!(error.contains(expect), "{text}\n→ {error}");
        }
    }

    /// The published schema and this parser describe the same file: every
    /// field either knows, the other knows, and the version is the latest.
    #[test]
    fn the_schema_matches_the_fields_this_build_reads() {
        let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
        let keys = |value: &serde_json::Value| -> Vec<String> {
            let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            keys
        };
        let mut top: Vec<String> = TOP_FIELDS.iter().map(|(f, _)| (*f).to_owned()).collect();
        top.sort();
        assert_eq!(keys(&schema["properties"]), top);
        let mut agent: Vec<String> = AGENT_FIELDS.iter().map(|(f, _)| (*f).to_owned()).collect();
        agent.sort();
        assert_eq!(
            keys(&schema["$defs"]["agent"]["properties"]),
            agent,
            "schemas/agentfile.schema.json lists every agent field"
        );
        assert_eq!(schema["properties"]["version"]["maximum"], LATEST);
        assert_eq!(schema["$id"], SCHEMA_URL);
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["$defs"]["agent"]["additionalProperties"], false);
        let runtimes: Vec<&str> = schema["$defs"]["agent"]["properties"]["runtime"]["examples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for runtime in launch::BUILT {
            assert!(
                runtimes.contains(runtime),
                "{runtime} is an example runtime"
            );
        }
    }
}
