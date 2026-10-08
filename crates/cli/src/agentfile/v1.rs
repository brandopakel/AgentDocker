//! Agentfile version 1, as it was: every file written before `version`
//! existed. It is read as written and upgraded in memory to the current
//! version, so an old file keeps meaning exactly what it meant. Never edit
//! this format; a change is a new version with an upgrader from the last.

use std::collections::BTreeMap;
use std::path::PathBuf;

use indexmap::IndexMap;
use serde::Deserialize;

use super::interpolate::escape;

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Agentfile {
    /// Accepted so a file can say `version = 1` outright; the caller has
    /// already read it.
    #[serde(default, rename = "version")]
    _version: Option<u32>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub agents: IndexMap<String, AgentEntry>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct AgentEntry {
    #[serde(default = "super::default_runtime")]
    pub runtime: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub command: Vec<String>,
    #[serde(default)]
    pub workdir: Option<PathBuf>,
    #[serde(default)]
    pub isolate: bool,
    #[serde(default)]
    pub tty: bool,
    #[serde(default)]
    pub restore: bool,
    #[serde(default)]
    pub in_pane: bool,
    #[serde(default)]
    pub restart: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

impl Agentfile {
    /// The same file as version 2. Version 1 never expanded `${VAR}`, so
    /// every `$` in a value that version 2 expands is doubled: a `$` here
    /// was always a `$`, and stays one.
    pub fn upgrade(self) -> super::Agentfile {
        let escape_map = |map: BTreeMap<String, String>| {
            map.into_iter()
                .map(|(key, value)| (key, escape(&value)))
                .collect()
        };
        super::Agentfile {
            version: 2,
            name: self.name,
            agents: self
                .agents
                .into_iter()
                .map(|(name, entry)| {
                    let entry = super::AgentEntry {
                        runtime: entry.runtime,
                        provider: entry.provider.as_deref().map(escape),
                        model: entry.model.as_deref().map(escape),
                        command: entry.command.iter().map(|word| escape(word)).collect(),
                        workdir: entry.workdir.map(|dir| escape(&dir.to_string_lossy())),
                        isolate: entry.isolate,
                        tty: entry.tty,
                        restore: entry.restore,
                        in_pane: entry.in_pane,
                        restart: entry.restart,
                        depends_on: entry.depends_on,
                        env: escape_map(entry.env),
                        labels: escape_map(entry.labels),
                        ..super::AgentEntry::default()
                    };
                    (name, entry)
                })
                .collect(),
        }
    }
}
