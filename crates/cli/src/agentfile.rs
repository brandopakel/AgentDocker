//! `Agentfile.toml`: several agents described together, the way a compose
//! file describes several containers.
//!
//! ```toml
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
//! after = "writer exits 0"        # or a list of such conditions
//! ```
//!
//! Agents start in the order they are written, except that one starts
//! after what it waits on: `depends_on` (started first) and `after` (ended
//! with status zero first). What those mean, and when an entry is never
//! started, is decided in `agentdocker_core::agentfile`.
//!
//! A file without `version` is version 1, the format before versions: it is
//! read as it was written and upgraded in memory, so it keeps meaning what
//! it meant. Each field records the version that introduced it
//! ([`AGENT_FIELDS`]), so a field a file's version does not have is refused
//! with the version to ask for rather than as unknown. Version 2 expands
//! `${VAR}` in values ([`agentdocker_core::agentfile::expand`]) and lets an
//! entry say what it wants (`prompt`, `model`, `effort`) instead of how to
//! type it, for the runtimes [`launch`] builds a command for.

pub mod launch;
pub mod upgrade;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use agentdocker_core::agentfile::expand::{escape, expand};
use agentdocker_core::agentfile::{self, After};
use agentdocker_core::{AgentSpec, Wiring};
use agentdocker_host::runtimes::Roots;
use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use serde::Deserialize;

/// Looked for in the current directory when no `-f` is given.
pub const DEFAULT_FILES: &[&str] = &["Agentfile.toml", "agentfile.toml"];

/// The newest format version this build reads and writes.
pub const LATEST: u32 = 2;

/// The JSON Schema of the latest version, for editors (`#:schema` on the
/// first line of a TOML file) and for `agentdocker agentfile schema`.
pub const SCHEMA: &str = include_str!("../schemas/agentfile.schema.json");

/// Where the schema is published: the file above, on the default branch.
pub const SCHEMA_URL: &str = "https://raw.githubusercontent.com/brandopakel/AgentDocker/main/crates/cli/schemas/agentfile.schema.json";

/// Top-level fields, each with the version that introduced it. `version`
/// itself is accepted in every version, so a file can pin itself to 1.
pub const TOP_FIELDS: &[(&str, u32)] = &[("version", 1), ("name", 1), ("agents", 1)];

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
    ("after", 1),
    ("env", 1),
    ("labels", 1),
    ("prompt", 2),
    ("effort", 2),
    ("args", 2),
    ("config", 2),
    ("idle_messages", 2),
    ("coordinate", 2),
];

/// The fields whose string values are expanded from version 2 on (and in
/// which an upgrade from version 1 doubles every `$`). Names, the runtime,
/// the restart policy and the waits are taken as written.
pub const EXPANDED_FIELDS: &[&str] = &[
    "provider", "model", "command", "workdir", "env", "labels", "prompt", "effort", "args",
    "config",
];

/// A file, as the latest version, whatever version it was written in.
#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Agentfile {
    /// Always [`LATEST`] once read; what the file said is `declared`.
    #[serde(default = "latest")]
    pub version: u32,
    /// The version the file declared (1 when it declared none), before the
    /// upgrade in memory: what `agentfile check` reports.
    #[serde(skip)]
    pub declared: u32,
    /// Team name; every agent gets a `team=<name>` label.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub agents: IndexMap<String, AgentEntry>,
}

fn latest() -> u32 {
    LATEST
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct AgentEntry {
    #[serde(default = "default_runtime")]
    pub runtime: String,
    #[serde(default)]
    pub provider: Option<String>,
    /// The model. On a command agentdocker builds it is passed to the
    /// runtime; on a written `command` it is a label for `ps`.
    #[serde(default)]
    pub model: Option<String>,
    /// The command line, launched as written. Required in version 1; from
    /// version 2 it may be left out for a runtime agentdocker builds a
    /// command for ([`launch::BUILT`]), and then must be.
    #[serde(default)]
    pub command: Vec<String>,
    /// Relative paths resolve against the Agentfile's directory.
    #[serde(default)]
    pub workdir: Option<String>,
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
    /// Agents that must have exited with status zero before this one
    /// starts: `"build exits 0"`, or a list of such conditions. If one
    /// ends any other way, this agent is never started, and nor is
    /// anything waiting on it.
    #[serde(default, deserialize_with = "one_or_many")]
    pub after: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Version 2: the first instruction, for a command agentdocker builds.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Version 2: reasoning effort, in the runtime's own words.
    #[serde(default)]
    pub effort: Option<String>,
    /// Version 2: the runtime's own arguments, added to a built command
    /// before the prompt.
    #[serde(default)]
    pub args: Vec<String>,
    /// Version 2: the runtime's own settings, given the way it takes them
    /// (Claude Code's `--settings`, Codex's `-c key=value`).
    #[serde(default)]
    pub config: BTreeMap<String, toml::Value>,
    /// Version 2: let a message start a turn while the session is idle
    /// (Claude Code's channel, Codex's input adapter; both experimental).
    #[serde(default)]
    pub idle_messages: bool,
    /// Version 2: wire AgentDocker into a built command where the
    /// runtime's own configuration lacks it (default true).
    #[serde(default)]
    pub coordinate: Option<bool>,
}

/// `after = "a exits 0"` or `after = ["a exits 0", "b exits 0"]`, as a list
/// either way.
fn one_or_many<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Conditions;
    impl<'de> serde::de::Visitor<'de> for Conditions {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a condition such as \"build exits 0\", or a list of them")
        }
        fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Self::Value, E> {
            Ok(vec![text.to_owned()])
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut all = Vec::new();
            while let Some(text) = seq.next_element::<String>()? {
                all.push(text);
            }
            Ok(all)
        }
    }
    deserializer.deserialize_any(Conditions)
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

    /// Whether this entry's command is one agentdocker builds: it wrote
    /// none.
    pub fn built(&self) -> bool {
        self.command.is_empty()
    }

    /// The entry with `${VAR}` expanded in every value that takes it
    /// ([`EXPANDED_FIELDS`]), read through `lookup`. The error names the
    /// field (`agents.writer.command[1]`).
    pub fn expanded(&self, name: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<Self> {
        let field = |key: &str| format!("agents.{name}.{key}");
        let one = |key: &str, value: &Option<String>| -> Result<Option<String>> {
            value
                .as_deref()
                .map(|text| expand(text, &field(key), lookup))
                .transpose()
                .map_err(anyhow::Error::msg)
        };
        let list = |key: &str, values: &[String]| -> Result<Vec<String>> {
            values
                .iter()
                .enumerate()
                .map(|(i, text)| expand(text, &format!("{}[{i}]", field(key)), lookup))
                .collect::<Result<_, _>>()
                .map_err(anyhow::Error::msg)
        };
        let map =
            |key: &str, values: &BTreeMap<String, String>| -> Result<BTreeMap<String, String>> {
                values
                    .iter()
                    .map(|(k, text)| {
                        expand(text, &format!("{}.{k}", field(key)), lookup)
                            .map(|value| (k.clone(), value))
                    })
                    .collect::<Result<_, _>>()
                    .map_err(anyhow::Error::msg)
            };
        let mut config = BTreeMap::new();
        for (key, value) in &self.config {
            config.insert(
                key.clone(),
                expand_value(value, &format!("{}.{key}", field("config")), lookup)?,
            );
        }
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
            config,
            ..self.clone()
        })
    }

    /// A version 1 entry as version 2: every `$` in a value version 2
    /// expands is doubled, so a `$` that was always a `$` stays one.
    fn escaped(mut self) -> Self {
        let escape_map = |map: &mut BTreeMap<String, String>| {
            for value in map.values_mut() {
                *value = escape(value);
            }
        };
        self.provider = self.provider.as_deref().map(escape);
        self.model = self.model.as_deref().map(escape);
        self.workdir = self.workdir.as_deref().map(escape);
        for word in &mut self.command {
            *word = escape(word);
        }
        escape_map(&mut self.env);
        escape_map(&mut self.labels);
        self
    }
}

/// `${VAR}` in the strings of a `config` value, however deep.
fn expand_value(
    value: &toml::Value,
    field: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<toml::Value> {
    Ok(match value {
        toml::Value::String(text) => {
            toml::Value::String(expand(text, field, lookup).map_err(anyhow::Error::msg)?)
        }
        toml::Value::Array(items) => toml::Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(i, item)| expand_value(item, &format!("{field}[{i}]"), lookup))
                .collect::<Result<_>>()?,
        ),
        toml::Value::Table(table) => toml::Value::Table(
            table
                .iter()
                .map(|(key, item)| {
                    expand_value(item, &format!("{field}.{key}"), lookup)
                        .map(|item| (key.clone(), item))
                })
                .collect::<Result<_>>()?,
        ),
        other => other.clone(),
    })
}

/// The version a file declares, refusing one this build cannot read.
pub fn declared_version(raw: &toml::Table) -> Result<u32> {
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
                "`{key}`{place} arrived in Agentfile version {since}; this file is version {version}, \
                 so add `version = {since}` at the top{}",
                if version == 1 {
                    " (from version 2, `$` in a value starts `${VAR}`: write `$$` for a dollar sign, \
                     or let `agentdocker agentfile upgrade` do it)"
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

/// What one entry became: the spec `run` is sent, and what building it had
/// to tell the person (an adapter not wired, a CLI not found).
#[derive(Clone, Debug)]
pub struct Built {
    pub spec: AgentSpec,
    pub notes: Vec<String>,
}

impl Agentfile {
    /// Read a file of any version this build knows, upgraded to the latest,
    /// and checked for everything that can be judged without the machine:
    /// `${VAR}` is expanded, and commands built, by [`Self::specs`].
    pub fn parse(text: &str) -> Result<Self> {
        let raw: toml::Table = toml::from_str(text)?;
        let version = declared_version(&raw)?;
        check_fields(&raw, version)?;
        // Read again from the text, so a type error names its line.
        let mut file: Self = toml::from_str(text)?;
        if version == 1 {
            // Version 1 never expanded a `$`; version 2 does.
            file.agents = file
                .agents
                .into_iter()
                .map(|(name, entry)| (name, entry.escaped()))
                .collect();
        }
        file.version = LATEST;
        file.declared = version;
        for (name, entry) in &file.agents {
            if name.is_empty() {
                bail!("agent names must not be empty");
            }
            if entry.command.first().is_some_and(String::is_empty) {
                bail!("agent `{name}` has an empty command");
            }
            if version == 1 && entry.command.is_empty() {
                bail!("agent `{name}` has an empty command");
            }
            launch::check(name, entry, launch::Judged::AsWritten).map_err(anyhow::Error::msg)?;
            if let Some(restart) = &entry.restart
                && agentdocker_core::RestartPolicy::parse(restart).is_none()
            {
                bail!(
                    "agent `{name}` has restart = \"{restart}\"; use no, always, on-failure, \
                     or on-failure:<n>"
                );
            }
            for condition in &entry.after {
                if let Err(why) = After::parse(condition) {
                    bail!("agent `{name}` has after = \"{condition}\": {why}");
                }
            }
        }
        // A wait that could never end — on a name that is not in the
        // file, on itself, round a ring — is a sentence now, with the
        // entry it is on, rather than an `up` that hangs.
        agentfile::check(&file.entries())?;
        Ok(file)
    }

    /// Each entry's ordering, in file order, for the rules in
    /// `agentdocker_core::agentfile`. Conditions are read when the file
    /// is parsed, so by here every one of them reads.
    pub fn entries(&self) -> Vec<agentfile::Entry> {
        self.agents
            .iter()
            .map(|(name, entry)| agentfile::Entry {
                name: name.clone(),
                depends_on: entry.depends_on.clone(),
                after: entry
                    .after
                    .iter()
                    .filter_map(|condition| After::parse(condition).ok())
                    .collect(),
                restart: entry.restart_policy(),
            })
            .collect()
    }

    /// The path `-f` names, or the first default file name in the current
    /// directory.
    pub fn locate(path: Option<&Path>) -> Result<PathBuf> {
        match path {
            Some(path) => Ok(path.to_path_buf()),
            None => DEFAULT_FILES
                .iter()
                .map(PathBuf::from)
                .find(|candidate| candidate.exists())
                .with_context(|| {
                    format!(
                        "no {} in the current directory (use -f)",
                        DEFAULT_FILES.join(" or ")
                    )
                }),
        }
    }

    /// Read `path`, or the first default file name in the current directory.
    /// Returns the file and its canonical path.
    pub fn load(path: Option<&Path>) -> Result<(Self, PathBuf)> {
        let path = Self::locate(path)?;
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let file = Self::parse(&text)
            .with_context(|| format!("{} is not a valid Agentfile", path.display()))?;
        let path = path.canonicalize().unwrap_or(path);
        Ok((file, path))
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

    /// What `up` works from for this file, and what `agentfile check`
    /// shows: every entry built, whichever `only` selects — an entry left
    /// out is still judged against the launch the file gives it now — once
    /// a name the file does not have has been refused. One function for
    /// both, so the check fails wherever `up` would, an unset `${VAR}` in
    /// an entry that was not named included.
    pub fn launches(
        &self,
        file_path: &Path,
        only: &[String],
        host: &dyn launch::Host,
    ) -> Result<Vec<Built>> {
        self.names(file_path, only)?;
        self.specs(file_path, &[], host)
    }

    /// Specs for the named agents (all when `only` is empty), in file
    /// order, with `${VAR}` expanded, working directories resolved, built
    /// commands built and bookkeeping labels (`agentfile`, `team`) added.
    pub fn specs(
        &self,
        file_path: &Path,
        only: &[String],
        host: &dyn launch::Host,
    ) -> Result<Vec<Built>> {
        let base = file_path.parent().unwrap_or(Path::new("."));
        let lookup = |name: &str| host.var(name);
        self.names(file_path, only)?
            .into_iter()
            .map(|name| {
                let entry = self.agents[&name].expanded(&name, &lookup)?;
                // What expansion produced is judged as written text was.
                launch::check(&name, &entry, launch::Judged::Expanded)
                    .map_err(anyhow::Error::msg)?;
                if entry.command.first().is_some_and(String::is_empty) {
                    bail!("agent `{name}` has an empty command once expanded");
                }
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
                let notes = launch::apply(&mut spec, &entry, host)
                    .with_context(|| format!("agent `{name}`"))?;
                Ok(Built { spec, notes })
            })
            .collect()
    }
}

/// The machine as it is, asked about each entry where it will run: the
/// environment `up` runs in with the entry's own `env` laid over it — so an
/// entry's `PATH` chooses its CLI and its `CLAUDE_CONFIG_DIR` or
/// `CODEX_HOME` the profile whose wiring counts — and the checkout the
/// session will hold.
pub struct SystemHost {
    /// The environment `up` runs in: what `${VAR}` reads, and what an
    /// entry's `env` is laid over.
    base: Vars,
    /// What has been found, by runtime, the roots it was looked for under
    /// and the directory the agent starts in, so two entries with different
    /// environments or places are asked about separately.
    found: RefCell<Vec<Found>>,
}

/// An environment as a process reads it: a variable's value, or `None`
/// when it is not set.
type Vars = Box<dyn Fn(&str) -> Option<OsString>>;

/// What was found for one runtime under one set of roots: its CLI, and
/// whether its configuration registers our MCP server.
struct Found {
    runtime: String,
    roots: Roots,
    start: PathBuf,
    cli: Option<PathBuf>,
    mcp: Wiring,
}

/// How long one `git` question about a checkout may take.
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The project settings files Claude Code reads in a session's working
/// directory.
const PROJECT_SETTINGS: [&str; 2] = [".claude/settings.json", ".claude/settings.local.json"];

impl SystemHost {
    pub fn new() -> Self {
        Self::with_env(|name| std::env::var_os(name))
    }

    /// A machine whose `up` runs with the environment `vars` answers.
    pub fn with_env(vars: impl Fn(&str) -> Option<OsString> + 'static) -> Self {
        Self {
            base: Box::new(vars),
            found: RefCell::default(),
        }
    }

    /// The roots the agent at `at` will see: its `env` over `up`'s, a
    /// relative directory in its own `env` taken from where it starts
    /// ([`launch::anchored`]).
    fn roots(&self, at: &launch::Destination) -> Roots {
        let mut roots = Roots::from_vars(&|name| match launch::entry_var(at.env, name) {
            Some(value) => Some(launch::anchored(name, value, at.workdir)),
            None => (self.base)(name),
        });
        // The versions are not needed, and each one is a process run.
        roots.versions = false;
        roots
    }

    fn found(&self, runtime: &str, at: &launch::Destination) -> (Option<PathBuf>, Wiring) {
        let roots = self.roots(at);
        if let Some(found) = self.found.borrow().iter().find(|found| {
            found.runtime == runtime && found.roots == roots && found.start == at.workdir
        }) {
            return (found.cli.clone(), found.mcp);
        }
        let (cli, mcp) = match agentdocker_core::runtime::spec(runtime) {
            Some(spec) => (
                agentdocker_host::runtimes::find_cli(spec, &roots),
                agentdocker_host::runtimes::mcp_wiring(spec, &roots, launch::MARKER),
            ),
            None => (None, Wiring::Unsupported),
        };
        self.found.borrow_mut().push(Found {
            runtime: runtime.to_owned(),
            roots,
            start: at.workdir.to_owned(),
            cli: cli.clone(),
            mcp,
        });
        (cli, mcp)
    }
}

impl Default for SystemHost {
    fn default() -> Self {
        Self::new()
    }
}

impl launch::Host for SystemHost {
    fn var(&self, name: &str) -> Option<String> {
        (self.base)(name).and_then(|value| value.into_string().ok())
    }

    fn program(&self, runtime: &str, at: &launch::Destination) -> Option<PathBuf> {
        self.found(runtime, at).0
    }

    fn agentdocker(&self) -> Result<PathBuf> {
        agentdocker_host::procinfo::executable_path()
            .context("cannot locate this agentdocker executable")
    }

    fn mcp_wired(&self, runtime: &str, at: &launch::Destination) -> bool {
        self.found(runtime, at).1 == Wiring::Wired
    }

    /// The user's settings, from the profile `at` names, and the project's
    /// as the session will find them: in `workdir` for an ordinary launch;
    /// for an isolated one, as the worktree the daemon makes will hold them
    /// ([`isolated_project_runs_hooks`]).
    fn claude_hooks_present(&self, at: &launch::Destination) -> bool {
        let Some(spec) = agentdocker_core::runtime::spec("claude-code") else {
            return false;
        };
        let user = agentdocker_host::runtimes::hook_config_path(spec, &self.roots(at));
        if agentdocker_host::runtimes::claude_settings_file_runs_hooks(&user, launch::MARKER) {
            return true;
        }
        if at.isolate {
            isolated_project_runs_hooks(at.workdir)
        } else {
            PROJECT_SETTINGS.iter().any(|file| {
                agentdocker_host::runtimes::claude_settings_file_runs_hooks(
                    &at.workdir.join(file),
                    launch::MARKER,
                )
            })
        }
    }
}

/// Whether the project settings a session in a worktree the daemon makes
/// for `workdir` will read run our hooks. The daemon makes that worktree
/// from the `HEAD` of the checkout `workdir` is in and starts the session
/// at its top level, so:
///
/// - a settings file `HEAD` tracks is what `HEAD` holds — an uncommitted
///   edit stays behind — and only a regular file counts: a tracked symlink
///   is not followed here, so the hooks are added rather than trusted;
/// - an untracked one is there only when `.worktreeinclude` has it copied,
///   by the daemon's own rules: a regular file (never a symlink), within
///   the copy's size budget, never written through anything in the new
///   worktree — whose tree is `HEAD`'s — that is not a directory, a
///   symlink or a file on its way, and on Unix only, where the daemon
///   copies at all.
///
/// False when `workdir` is in no checkout or git cannot say.
fn isolated_project_runs_hooks(workdir: &Path) -> bool {
    let git = |dir: &Path, args: Vec<String>| -> Option<String> {
        let mut argv = vec!["git".to_owned()];
        argv.extend(args);
        let output = agentdocker_host::command::run(dir, &argv, GIT_TIMEOUT).ok()?;
        output.success.then_some(output.stdout)
    };
    let words =
        |args: &[&str]| -> Vec<String> { args.iter().map(|arg| (*arg).to_owned()).collect() };
    let Some(top) = git(workdir, words(&["rev-parse", "--show-toplevel"])) else {
        return false;
    };
    let top = PathBuf::from(top.trim_end_matches(['\n', '\r']));
    // The mode `HEAD` gives each path under `.claude`, and `.claude` itself:
    // what the new worktree's checkout will put there.
    let tracked: BTreeMap<String, String> = git(
        &top,
        words(&["ls-tree", "-r", "-t", "-z", "HEAD", "--", ".claude"]),
    )
    .map(|listing| {
        listing
            .split('\0')
            .filter_map(|record| {
                let (meta, path) = record.split_once('\t')?;
                Some((path.to_owned(), meta.split(' ').next()?.to_owned()))
            })
            .collect()
    })
    .unwrap_or_default();
    let copied: Vec<String> = if cfg!(unix) {
        std::fs::read_to_string(top.join(agentdocker_host::vcs::WORKTREE_INCLUDE))
            .ok()
            .and_then(|text| agentdocker_host::vcs::worktree_include_listing(&text))
            .and_then(|args| git(&top, args))
            .map(|listed| {
                agentdocker_host::vcs::worktree_include_sources(&top, &listed)
                    .0
                    .into_iter()
                    .map(|(relative, _)| relative)
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    PROJECT_SETTINGS
        .iter()
        .any(|file| match tracked.get(*file).map(String::as_str) {
            Some("100644" | "100755") => {
                git(&top, words(&["cat-file", "blob", &format!("HEAD:{file}")])).is_some_and(
                    |text| {
                        agentdocker_host::runtimes::claude_settings_run_hooks(&text, launch::MARKER)
                    },
                )
            }
            Some(_) => false,
            None => {
                copied.iter().any(|path| path == file)
                    && agentdocker_host::vcs::worktree_include_writable(file, |at| {
                        // `HEAD` checks out a tree as a directory; anything
                        // else there — a symlink, a file — blocks the way.
                        at.to_str()
                            .and_then(|at| tracked.get(at))
                            .is_some_and(|mode| mode != "040000")
                    })
                    && agentdocker_host::runtimes::claude_settings_file_runs_hooks(
                        &top.join(file),
                        launch::MARKER,
                    )
            }
        })
}

#[cfg(test)]
mod tests {
    use super::launch::Host;
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
            .map(|built| built.spec)
            .collect()
    }

    #[test]
    fn parses_in_file_order_with_defaults() {
        let file = Agentfile::parse(SAMPLE).unwrap();
        assert_eq!(file.version, LATEST, "upgraded in memory");
        assert_eq!(file.declared, 1);
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
        let no_command = Agentfile::parse("version = 2\n[agents.a]\nruntime = \"aider\"\n")
            .unwrap_err()
            .to_string();
        assert!(no_command.contains("no `command`"), "{no_command}");
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
        assert_eq!(Agentfile::parse("version = 1\n").unwrap().declared, 1);
        assert_eq!(Agentfile::parse("").unwrap().declared, 1);
        assert_eq!(Agentfile::parse("version = 2\n").unwrap().declared, 2);
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
        // Stopping needs names only, so a file with an unset variable
        // still names its agents.
        let names = Agentfile::parse("version = 2\n[agents.a]\ncommand = [\"${NOPE}\"]\n")
            .unwrap()
            .names(Path::new("/x/A.toml"), &[])
            .unwrap();
        assert_eq!(names, ["a"]);
    }

    /// The names in the order `up` considers them.
    fn ordered(file: &Agentfile) -> Vec<String> {
        let entries = file.entries();
        agentfile::order(&entries)
            .into_iter()
            .map(|i| entries[i].name.clone())
            .collect()
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
        assert_eq!(ordered(&file), ["db", "cache", "web"]);
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
        // Not sorted: the order somebody wrote them in is information.
        assert_eq!(ordered(&file), ["zeta", "alpha"]);
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

    #[test]
    fn after_reads_one_condition_or_a_list() {
        let file = Agentfile::parse(
            r#"
[agents.build]
command = ["true"]

[agents.lint]
command = ["true"]

[agents.test]
command = ["true"]
after = "build exits 0"

[agents.deploy]
command = ["true"]
after = ["test exits 0", "  lint   exits 0 "]
"#,
        )
        .unwrap();
        let entries = file.entries();
        let after = |name: &str| -> Vec<String> {
            entries
                .iter()
                .find(|e| e.name == name)
                .unwrap()
                .after
                .iter()
                .map(|a| a.name.clone())
                .collect()
        };
        assert_eq!(after("test"), ["build"]);
        assert_eq!(after("deploy"), ["test", "lint"]);
        assert!(after("build").is_empty(), "no after is no wait");
        assert_eq!(ordered(&file), ["build", "lint", "test", "deploy"]);
        // A quoted name with a space in it reads too.
        let spaced = Agentfile::parse(
            r#"
[agents."db seed"]
command = ["true"]

[agents.web]
command = ["true"]
after = "db seed exits 0"
"#,
        )
        .unwrap();
        assert_eq!(spaced.entries()[1].after[0].name, "db seed");
    }

    /// The error `parse` gives for a file, as one line.
    fn refused(text: &str) -> String {
        format!("{:#}", Agentfile::parse(text).unwrap_err())
    }

    #[test]
    fn after_is_refused_when_its_wait_could_never_end_and_the_entry_is_named() {
        let unknown = refused(
            r#"
[agents.test]
command = ["true"]
after = "biuld exits 0"
"#,
        );
        assert!(unknown.contains("`test`"), "{unknown}");
        assert!(unknown.contains("`biuld` is not in this file"), "{unknown}");

        let itself = refused(
            r#"
[agents.test]
command = ["true"]
after = "test exits 0"
"#,
        );
        assert!(itself.contains("`test`"), "{itself}");
        assert!(itself.contains("waits on itself"), "{itself}");

        let ring = refused(
            r#"
[agents.a]
command = ["true"]
after = "c exits 0"

[agents.b]
command = ["true"]
after = "a exits 0"

[agents.c]
command = ["true"]
after = ["b exits 0"]
"#,
        );
        assert!(ring.contains("cycle: a → c → b → a"), "{ring}");

        // A ring can go through both kinds of wait.
        let mixed = refused(
            r#"
[agents.a]
command = ["true"]
depends_on = ["b"]

[agents.b]
command = ["true"]
after = "a exits 0"
"#,
        );
        assert!(mixed.contains("cycle"), "{mixed}");

        let never = refused(
            r#"
[agents.serve]
command = ["true"]
restart = "always"

[agents.probe]
command = ["true"]
after = "serve exits 0"
"#,
        );
        assert!(never.contains("`probe`"), "{never}");
        assert!(never.contains("restart = \"always\""), "{never}");
    }

    #[test]
    fn a_malformed_condition_is_named_with_its_entry() {
        for (condition, says) in [
            ("\"build\"", "not a condition"),
            ("\"build exits 1\"", "only exit status 0"),
            ("\"build ends 0\"", "`ends` is not `exits`"),
            ("\"\"", "it is empty"),
            ("[\"build exits 0\", \"exits 0\"]", "names no entry"),
        ] {
            let error = refused(&format!(
                "[agents.build]\ncommand = [\"true\"]\n\n[agents.test]\ncommand = [\"true\"]\nafter = {condition}\n"
            ));
            assert!(
                error.contains("agent `test` has after = "),
                "{condition}: {error}"
            );
            assert!(error.contains(says), "{condition}: {error}");
        }
        // The wrong type is TOML's error, which names the line.
        let typed = refused("[agents.test]\ncommand = [\"true\"]\nafter = 5\n");
        assert!(typed.contains("line 3"), "{typed}");
        assert!(
            typed.contains("build exits 0"),
            "says what it wants: {typed}"
        );
    }

    /// `${VAR}` reaches every value that takes it, `config` however deep
    /// included, and nothing else: the runtime, the waits and the names
    /// are what the file says.
    #[test]
    fn expansion_reaches_every_expanded_field_and_no_other() {
        let file = Agentfile::parse(
            r#"
version = 2
[agents.w]
runtime = "codex"
provider = "${MODEL}"
model = "${MODEL}"
prompt = "do ${TASK}"
effort = "${EFFORT:-low}"
args = ["--${MODEL}"]
workdir = "${MODEL}"
config = { approval_policy = "${MODEL}", nested = { list = ["${TASK}"] } }
env = { A = "${MODEL}" }
labels = { b = "${TASK}" }
depends_on = []
"#,
        )
        .unwrap();
        let entry = file.agents["w"]
            .expanded("w", &|name| Fake::default().var(name))
            .unwrap();
        assert_eq!(entry.provider.as_deref(), Some("opus"));
        assert_eq!(entry.model.as_deref(), Some("opus"));
        assert_eq!(entry.prompt.as_deref(), Some("do the parser"));
        assert_eq!(entry.effort.as_deref(), Some("low"));
        assert_eq!(entry.args, ["--opus"]);
        assert_eq!(entry.workdir.as_deref(), Some("opus"));
        assert_eq!(entry.config["approval_policy"], toml::Value::from("opus"));
        assert_eq!(
            entry.config["nested"]["list"][0],
            toml::Value::from("the parser")
        );
        assert_eq!(entry.env["A"], "opus");
        assert_eq!(entry.labels["b"], "the parser");
        assert_eq!(entry.runtime, "codex");
        let deep = Agentfile::parse(
            "version = 2\n[agents.w]\nruntime = \"codex\"\ntty = true\nconfig = { nested = { list = [\"${NOPE}\"] } }\n",
        )
        .unwrap()
        .agents["w"]
            .expanded("w", &|_| None)
            .unwrap_err()
            .to_string();
        assert!(
            deep.contains("agents.w.config.nested.list[0]"),
            "names the place: {deep}"
        );
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
        // Every expanded field is a string, or holds strings, in the schema.
        for field in EXPANDED_FIELDS {
            assert!(
                AGENT_FIELDS.iter().any(|(f, _)| f == field),
                "{field} is a field"
            );
            let property = &schema["$defs"]["agent"]["properties"][*field];
            let expandable = |node: &serde_json::Value| node["$ref"] == "#/$defs/expandable";
            assert!(
                expandable(property)
                    || expandable(&property["items"])
                    || expandable(&property["additionalProperties"])
                    || property["description"]
                        .as_str()
                        .is_some_and(|text| text.contains("${VAR}")),
                "{field} says it expands"
            );
        }
    }

    /// A machine of files: `up`'s environment is `vars`, nothing else.
    fn machine(vars: &[(&str, &Path)]) -> SystemHost {
        let vars: BTreeMap<String, OsString> = vars
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.as_os_str().to_owned()))
            .collect();
        SystemHost::with_env(move |name| vars.get(name).cloned())
    }

    /// Claude Code settings that run every one of our hooks.
    fn wired_settings(exe: &Path) -> String {
        let mut settings = serde_json::json!({});
        crate::hooks::merge_claude_code_hooks(
            &mut settings,
            &agentdocker_host::runtimes::claude_hook_command(exe).unwrap(),
        )
        .unwrap();
        settings.to_string()
    }

    /// The command one Claude Code entry gets on `host`, with `extra` lines
    /// in its table, from an Agentfile in `dir`.
    fn claude_command(host: &SystemHost, dir: &Path, extra: &str) -> Vec<String> {
        let text = format!(
            "version = 2\n[agents.w]\nruntime = \"claude-code\"\nprompt = \"go\"\n{extra}\n"
        );
        Agentfile::parse(&text)
            .unwrap()
            .specs(&dir.join("Agentfile.toml"), &[], host)
            .unwrap()
            .remove(0)
            .spec
            .command
    }

    /// Whether a built `--settings` document runs every one of our hooks,
    /// judged as the inventory judges a settings file. Its commands run
    /// this test binary, which is not named `agentdocker`, so they are
    /// judged as they would be from the installed one.
    fn runs_our_hooks(settings: &str) -> bool {
        let me = agentdocker_host::runtimes::claude_hook_command(
            &agentdocker_host::procinfo::executable_path().unwrap(),
        )
        .unwrap();
        let installed =
            agentdocker_host::runtimes::claude_hook_command(Path::new("/opt/ad/agentdocker"))
                .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(settings).unwrap();
        for entries in value["hooks"]
            .as_object_mut()
            .into_iter()
            .flat_map(|events| events.values_mut())
        {
            for entry in entries.as_array_mut().into_iter().flatten() {
                for hook in entry["hooks"].as_array_mut().into_iter().flatten() {
                    if hook["command"] == me.as_str() {
                        hook["command"] = installed.clone().into();
                    }
                }
            }
        }
        agentdocker_host::runtimes::claude_settings_run_hooks(&value.to_string(), launch::MARKER)
    }

    fn settings_of(command: &[String]) -> Option<&str> {
        command
            .iter()
            .position(|word| word == "--settings")
            .map(|at| command[at + 1].as_str())
    }

    /// The profile an entry's own `env` names is the one whose wiring
    /// counts: with the person's default profile wired, an entry pointing
    /// `CLAUDE_CONFIG_DIR` at an unwired one gets the MCP server and the
    /// hooks in its launch, and one that names no profile gets neither.
    #[test]
    fn an_entrys_own_profile_decides_what_is_wired_into_its_launch() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let exe = Path::new("/opt/ad/agentdocker");
        std::fs::write(
            home.join(".claude.json"),
            serde_json::json!({"mcpServers": {"agentdocker": {
                "type": "stdio", "command": exe, "args": ["mcp", "--runtime", "claude-code"]
            }}})
            .to_string(),
        )
        .unwrap();
        std::fs::write(home.join(".claude/settings.json"), wired_settings(exe)).unwrap();
        let profile = temp.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let host = machine(&[("HOME", home.as_path()), ("USERPROFILE", home.as_path())]);

        let default = claude_command(&host, temp.path(), "");
        assert!(!default.contains(&"--mcp-config".to_owned()), "{default:?}");
        assert_eq!(settings_of(&default), None, "{default:?}");

        let own = claude_command(
            &host,
            temp.path(),
            &format!(
                "env = {{ CLAUDE_CONFIG_DIR = {:?} }}",
                profile.display().to_string()
            ),
        );
        let mcp = own
            .iter()
            .position(|word| word == "--mcp-config")
            .expect("the unwired profile gets the MCP server");
        assert!(own[mcp + 1].contains(r#""mcp","--runtime","claude-code""#));
        let settings = settings_of(&own).expect("and the hooks");
        assert!(runs_our_hooks(settings), "{settings}");
    }

    /// An entry's own `PATH` chooses its CLI, and what was found for one
    /// environment is not reused for another.
    #[cfg(unix)]
    #[test]
    fn an_entrys_own_path_chooses_its_cli() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        for dir in ["a", "b"] {
            let bin = temp.path().join(dir);
            std::fs::create_dir_all(&bin).unwrap();
            let cli = bin.join("claude");
            std::fs::write(&cli, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let home = temp.path().join("home");
        let first = temp.path().join("a");
        let host = machine(&[("HOME", home.as_path()), ("PATH", first.as_path())]);
        assert_eq!(
            claude_command(&host, temp.path(), "")[0],
            first.join("claude").display().to_string()
        );
        let second = temp.path().join("b");
        assert_eq!(
            claude_command(
                &host,
                temp.path(),
                &format!("env = {{ PATH = {:?} }}", second.display().to_string())
            )[0],
            second.join("claude").display().to_string()
        );
    }

    /// A relative directory first in an entry's own `PATH` is found in the
    /// directory the agent starts in, and chooses its CLI there.
    #[cfg(unix)]
    #[test]
    fn an_entrys_relative_path_is_searched_where_it_starts() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let workdir = temp.path().join("w");
        for bin in [workdir.join("bin"), temp.path().join("a")] {
            std::fs::create_dir_all(&bin).unwrap();
            let cli = bin.join("claude");
            std::fs::write(&cli, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let home = temp.path().join("home");
        let other = temp.path().join("a");
        let host = machine(&[("HOME", home.as_path()), ("PATH", other.as_path())]);
        let path = format!("bin:{}", other.display());
        let command = claude_command(&host, &workdir, &format!("env = {{ PATH = {path:?} }}"));
        assert_eq!(
            command[0],
            workdir
                .canonicalize()
                .unwrap()
                .join("bin/claude")
                .display()
                .to_string()
        );
    }

    /// A relative `CLAUDE_CONFIG_DIR` in an entry's own `env` is the
    /// profile at the agent's working directory, wherever `up` runs: wired
    /// there, nothing is added; the same words for an agent elsewhere name
    /// another, unwired profile, which gets the MCP server and the hooks.
    #[test]
    fn an_entrys_relative_profile_is_read_where_it_starts() {
        let temp = tempfile::tempdir().unwrap();
        let workdir = temp.path().join("w");
        let profile = workdir.join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let exe = Path::new("/opt/ad/agentdocker");
        std::fs::write(
            profile.join(".claude.json"),
            serde_json::json!({"mcpServers": {"agentdocker": {
                "type": "stdio", "command": exe, "args": ["mcp", "--runtime", "claude-code"]
            }}})
            .to_string(),
        )
        .unwrap();
        std::fs::write(profile.join("settings.json"), wired_settings(exe)).unwrap();
        assert!(
            !Path::new("profile").exists(),
            "the test's own directory has no such profile"
        );
        let home = temp.path().join("home");
        let host = machine(&[("HOME", home.as_path()), ("USERPROFILE", home.as_path())]);
        let own = "env = { CLAUDE_CONFIG_DIR = \"profile\" }";
        let here = claude_command(&host, &workdir, own);
        assert!(!here.contains(&"--mcp-config".to_owned()), "{here:?}");
        assert_eq!(settings_of(&here), None, "{here:?}");
        let elsewhere = temp.path().join("v");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let there = claude_command(&host, &elsewhere, own);
        assert!(there.contains(&"--mcp-config".to_owned()), "{there:?}");
        assert!(settings_of(&there).is_some_and(runs_our_hooks), "{there:?}");
    }

    fn have_git() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn git(home: &Path, dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// An isolated agent runs in a worktree the daemon makes from `HEAD`,
    /// so its project settings are the committed ones: hooks that are only
    /// an uncommitted edit count for an ordinary launch and not for an
    /// isolated one, which gets them in its launch; an ignored local
    /// settings file that `.worktreeinclude` has copied counts for both.
    #[test]
    fn an_isolated_launch_counts_the_settings_its_worktree_will_hold() {
        if !have_git() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        git(&home, &repo, &["init", "-q"]);
        std::fs::write(repo.join(".claude/settings.json"), "{}\n").unwrap();
        git(&home, &repo, &["add", ".claude/settings.json"]);
        git(&home, &repo, &["commit", "-q", "-m", "shared settings"]);
        let exe = Path::new("/opt/ad/agentdocker");
        std::fs::write(repo.join(".claude/settings.json"), wired_settings(exe)).unwrap();
        let host = machine(&[("HOME", home.as_path()), ("USERPROFILE", home.as_path())]);

        let here = claude_command(&host, &repo, "");
        assert_eq!(settings_of(&here), None, "the edit is here: {here:?}");
        let isolated = claude_command(&host, &repo, "isolate = true");
        let settings = settings_of(&isolated).expect("the worktree has only what HEAD has");
        assert!(runs_our_hooks(settings), "{settings}");

        #[cfg(unix)]
        {
            // The daemon copies a listed regular file, and never a symlink,
            // so only the second of these is in the worktree.
            std::fs::write(repo.join(".gitignore"), ".claude/settings.local.json\n").unwrap();
            std::fs::write(
                repo.join(".worktreeinclude"),
                ".claude/settings.local.json\n",
            )
            .unwrap();
            let elsewhere = temp.path().join("wired.json");
            std::fs::write(&elsewhere, wired_settings(exe)).unwrap();
            let local = repo.join(".claude/settings.local.json");
            std::os::unix::fs::symlink(&elsewhere, &local).unwrap();
            let linked = claude_command(&host, &repo, "isolate = true");
            assert!(
                settings_of(&linked).is_some_and(runs_our_hooks),
                "a symlink is not copied: {linked:?}"
            );
            std::fs::remove_file(&local).unwrap();
            std::fs::write(&local, wired_settings(exe)).unwrap();
            let copied = claude_command(&host, &repo, "isolate = true");
            assert_eq!(settings_of(&copied), None, "a regular file is: {copied:?}");
        }
    }

    /// An include the new worktree cannot hold is not counted: `HEAD` has
    /// `.claude` as a file, so the copy of a listed
    /// `.claude/settings.local.json` fails there, and the hooks are added
    /// although the checkout here has them in a directory of that name.
    #[cfg(unix)]
    #[test]
    fn an_isolated_launch_does_not_trust_an_include_its_worktree_cannot_hold() {
        if !have_git() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&home, &repo, &["init", "-q"]);
        std::fs::write(repo.join(".claude"), "a file, in HEAD\n").unwrap();
        std::fs::write(repo.join(".gitignore"), ".claude/settings.local.json\n").unwrap();
        std::fs::write(
            repo.join(".worktreeinclude"),
            ".claude/settings.local.json\n",
        )
        .unwrap();
        git(&home, &repo, &["add", "."]);
        git(
            &home,
            &repo,
            &["commit", "-q", "-m", "a file named .claude"],
        );
        let exe = Path::new("/opt/ad/agentdocker");
        std::fs::remove_file(repo.join(".claude")).unwrap();
        std::fs::create_dir(repo.join(".claude")).unwrap();
        std::fs::write(
            repo.join(".claude/settings.local.json"),
            wired_settings(exe),
        )
        .unwrap();
        let host = machine(&[("HOME", home.as_path()), ("USERPROFILE", home.as_path())]);
        let here = claude_command(&host, &repo, "");
        assert_eq!(
            settings_of(&here),
            None,
            "here, the directory is real: {here:?}"
        );
        let isolated = claude_command(&host, &repo, "isolate = true");
        assert!(
            settings_of(&isolated).is_some_and(runs_our_hooks),
            "there, `.claude` is a file: {isolated:?}"
        );
    }

    /// Every fenced `toml` block with an `[agents.` table in the guide and
    /// on the front page, dedented, with where it came from.
    fn documented_examples() -> Vec<(String, String)> {
        let mut examples = Vec::new();
        for (name, text) in [
            ("docs/GUIDE.md", include_str!("../../../docs/GUIDE.md")),
            ("README.md", include_str!("../../../README.md")),
        ] {
            let mut block: Option<(usize, String)> = None;
            for (number, line) in text.lines().enumerate() {
                match &mut block {
                    None if line.trim() == "```toml" => {
                        block = Some((line.len() - line.trim_start().len(), String::new()));
                    }
                    Some((_, body)) if line.trim() == "```" => {
                        if body.contains("[agents.") {
                            examples.push((format!("{name}:{}", number + 1), body.clone()));
                        }
                        block = None;
                    }
                    Some((indent, body)) => {
                        let dedented = line.get(*indent..).unwrap_or(line.trim_start());
                        body.push_str(dedented);
                        body.push('\n');
                    }
                    None => {}
                }
            }
        }
        examples
    }

    /// A validator for the keywords the schema uses, so the examples can be
    /// held to it without a JSON Schema crate. An unknown keyword or pattern
    /// fails loudly rather than passing silently.
    fn validate(
        root: &serde_json::Value,
        schema: &serde_json::Value,
        value: &serde_json::Value,
        at: &str,
        errors: &mut Vec<String>,
    ) {
        use serde_json::Value as J;
        let schema = match schema.get("$ref").and_then(J::as_str) {
            Some(reference) => {
                let name = reference
                    .strip_prefix("#/$defs/")
                    .unwrap_or_else(|| panic!("{reference}: only #/$defs references"));
                &root["$defs"][name]
            }
            None => schema,
        };
        let object = schema.as_object().expect("a schema is an object");
        for (keyword, rule) in object {
            match keyword.as_str() {
                "$schema" | "$id" | "title" | "description" | "default" | "examples" | "$defs"
                | "$ref" => {}
                "type" => {
                    let ok = match rule.as_str().unwrap() {
                        "object" => value.is_object(),
                        "string" => value.is_string(),
                        "array" => value.is_array(),
                        "boolean" => value.is_boolean(),
                        "integer" => value.is_i64(),
                        other => panic!("type {other}"),
                    };
                    if !ok {
                        errors.push(format!("{at}: not {rule}"));
                    }
                }
                "properties" => {
                    if let Some(map) = value.as_object() {
                        for (key, item) in map {
                            if let Some(sub) = rule.get(key) {
                                validate(root, sub, item, &format!("{at}.{key}"), errors);
                            }
                        }
                    }
                }
                "additionalProperties" => {
                    if let Some(map) = value.as_object() {
                        let known = object.get("properties").and_then(J::as_object);
                        for (key, item) in map {
                            if known.is_some_and(|known| known.contains_key(key)) {
                                continue;
                            }
                            if rule == &J::Bool(false) {
                                errors.push(format!("{at}.{key}: not a known field"));
                            } else {
                                validate(root, rule, item, &format!("{at}.{key}"), errors);
                            }
                        }
                    }
                }
                "propertyNames" => {
                    if let Some(map) = value.as_object() {
                        let min = rule["minLength"].as_u64().unwrap_or(0) as usize;
                        for key in map.keys() {
                            if key.chars().count() < min {
                                errors.push(format!("{at}: key {key:?} is too short"));
                            }
                        }
                    }
                }
                "items" => {
                    if let Some(items) = value.as_array() {
                        for (i, item) in items.iter().enumerate() {
                            validate(root, rule, item, &format!("{at}[{i}]"), errors);
                        }
                    }
                }
                "minItems" => {
                    if value
                        .as_array()
                        .is_some_and(|items| (items.len() as u64) < rule.as_u64().unwrap())
                    {
                        errors.push(format!("{at}: too few items"));
                    }
                }
                "uniqueItems" => {
                    if let Some(items) = value.as_array() {
                        for (i, item) in items.iter().enumerate() {
                            if items[..i].contains(item) {
                                errors.push(format!("{at}[{i}]: repeated"));
                            }
                        }
                    }
                }
                "minimum" | "maximum" => {
                    if let (Some(n), Some(bound)) = (value.as_i64(), rule.as_i64())
                        && ((keyword == "minimum" && n < bound)
                            || (keyword == "maximum" && n > bound))
                    {
                        errors.push(format!("{at}: {n} is outside {keyword} {bound}"));
                    }
                }
                "pattern" => {
                    if let Some(text) = value.as_str() {
                        // The patterns the schema uses, each checked by
                        // the parser it mirrors.
                        let ok = match rule.as_str().unwrap() {
                            "^(no|always|on-failure(:[0-9]+)?)$" => {
                                agentdocker_core::RestartPolicy::parse(text).is_some()
                            }
                            "^.+ exits 0$" => After::parse(text).is_ok(),
                            other => panic!("pattern {other} is not one this test knows"),
                        };
                        if !ok {
                            errors.push(format!("{at}: {text:?} does not match {rule}"));
                        }
                    }
                }
                "oneOf" => {
                    let matching = rule
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|option| {
                            let mut inner = Vec::new();
                            validate(root, option, value, at, &mut inner);
                            inner.is_empty()
                        })
                        .count();
                    if matching != 1 {
                        errors.push(format!("{at}: matches {matching} of the alternatives"));
                    }
                }
                other => panic!("keyword {other} is not one this test knows"),
            }
        }
    }

    /// Every Agentfile the guide and the front page show reads with this
    /// parser and is valid under the published schema, and the schema
    /// still refuses what it should.
    #[test]
    fn the_documented_examples_read_and_validate_against_the_schema() {
        let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
        let examples = documented_examples();
        assert!(
            examples.len() >= 2,
            "the guide and the front page each show an Agentfile"
        );
        let mut versions_seen = std::collections::BTreeSet::new();
        for (place, text) in &examples {
            let file = Agentfile::parse(text).unwrap_or_else(|e| panic!("{place}: {e:#}\n{text}"));
            versions_seen.insert(file.declared);
            let raw: toml::Value = toml::from_str(text).unwrap();
            let json = serde_json::to_value(raw).unwrap();
            let mut errors = Vec::new();
            validate(&schema, &schema, &json, "file", &mut errors);
            assert!(errors.is_empty(), "{place}: {errors:#?}\n{text}");
        }
        assert!(
            versions_seen.contains(&2),
            "the documents show the current version"
        );
        // The validator does refuse: an unknown field, a bad restart, a
        // field of the wrong type, a bad condition.
        for (text, says) in [
            (
                "[agents.a]\ncommand = [\"x\"]\nbogus = 1\n",
                "not a known field",
            ),
            (
                "[agents.a]\ncommand = [\"x\"]\nrestart = \"sometimes\"\n",
                "does not match",
            ),
            ("[agents.a]\ncommand = \"x\"\n", "not \"array\""),
            ("[agents.a]\ncommand = []\n", "too few"),
            ("version = 3\n", "outside maximum"),
            (
                "[agents.a]\ncommand = [\"x\"]\nafter = \"b\"\n",
                "alternatives",
            ),
            (
                "[agents.a]\ncommand = [\"x\"]\ndepends_on = [\"b\", \"b\"]\n",
                "repeated",
            ),
        ] {
            let raw: toml::Value = toml::from_str(text).unwrap();
            let mut errors = Vec::new();
            validate(
                &schema,
                &schema,
                &serde_json::to_value(raw).unwrap(),
                "file",
                &mut errors,
            );
            assert!(
                errors.iter().any(|e| e.contains(says)),
                "{text}\n→ {errors:?}"
            );
        }
    }
}
