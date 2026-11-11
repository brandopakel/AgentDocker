//! Docker Agent's MCP wiring. Docker Agent (`docker-agent`, formerly
//! `cagent`; `docker agent` as a Docker CLI plugin) runs the model loop
//! itself from a YAML file per agent, and reads each agent's MCP servers
//! from that agent's `toolsets` in that file rather than from a file of
//! its own. So there is nothing for setup to register once for the
//! machine: AgentDocker's MCP server is a toolset entry in the YAML of
//! each agent that should coordinate, and this module is the pure text
//! edit that adds it and takes it out again.
//!
//! The shape, as Docker Agent's documentation for the `mcp` toolset gives
//! it (the `command` form, beside `ref` and `remote`):
//!
//! ```yaml
//! agents:
//!   root:
//!     model: openai/gpt-5-mini
//!     toolsets:
//!       - type: mcp
//!         command: /path/to/agentdocker
//!         args: ["mcp", "--runtime", "docker-agent"]
//! ```
//!
//! The edit is made by lines, so that everything the person wrote —
//! comments, blank lines, quoting, key order — stays byte for byte, and
//! only the lines of our entry (and a `toolsets:` key when the agent had
//! none) are added or removed. Two things keep a line edit honest.
//!
//! It is conservative by rule. Anywhere in the file outside block-scalar
//! text it refuses an anchor (`&name`), an alias (`*name`), a merge key
//! (`<<`, `!!merge`), a tag (`!tag`) and an explicit key (`? `): Docker
//! Agent's YAML reader resolves anchors and merges, the parser below does
//! not, and what such a file says is not on its lines. It refuses a quoted
//! key among the agents and in any agent's own keys, which a YAML reader
//! decodes and these lines do not; and in the agent it edits, a quoted
//! value or an inline list or mapping that runs over several lines, an
//! inline `toolsets`, an agent written as a flow mapping, and tabs.
//!
//! And every edit is confirmed before it is returned: the original and the
//! edited text are both read with a YAML parser (yaml-rust2), which must
//! find one document in each and no key twice in a mapping; the agent the
//! lines chose must be the one that reading names as the default; and the
//! edited document must equal the original but for our entry, added to
//! that agent's `toolsets` or taken out of them. What is refused or cannot
//! be confirmed is said with the reason, and the person adds the entry by
//! hand from the text [`docker_agent_toolset`] prints.
//!
//! A block scalar (`instruction: |`) is read as the one value it is, by
//! its content indentation — the indentation indicator's, else its first
//! line's — so a `#` or `- ` line in it is text and the entry never lands
//! inside it.
//!
//! The target is the agent `docker agent run <file>` runs when no
//! `--agent` names another: the one named `root`, else the first (Docker
//! Agent's `Team.DefaultAgent`). An agent with `harness:` set runs an
//! external coding CLI instead of a model, and Docker Agent documents that
//! it ignores that agent's `toolsets`; the edit still goes in, and the
//! caller is told so it can say.

use std::fmt;

use yaml_rust2::{Yaml, YamlLoader, yaml::Hash};

/// The runtime name Docker Agent sessions register with.
pub const RUNTIME: &str = "docker-agent";

/// Why a YAML file could not be edited; the person edits it by hand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// No `agents:` mapping at the top level, or an empty one.
    NoAgents,
    /// Tabs in the indentation of the lines the edit needs to read.
    Tabs,
    /// The agent is not written as a block mapping (`root:` with its keys
    /// indented beneath it).
    AgentNotBlock(String),
    /// The agent's `toolsets` is a flow sequence or a scalar the editor
    /// does not rewrite.
    ToolsetsNotBlock(String),
    /// YAML the editor refuses, at a line of the file (counted from 1):
    /// an anchor, an alias, a merge key, a tag or an explicit key or value
    /// anywhere; a quoted key among the agents or an agent's own keys; in
    /// the agent it edits, a quoted value or an inline list or mapping
    /// that runs over several lines.
    Construct { line: usize, form: &'static str },
    /// The edit could not be confirmed against a YAML reading of the file:
    /// what did not agree.
    Unverified(String),
}

/// The YAML the editor refuses, as its refusals name it.
const ANCHOR: &str = "an anchor (`&name`)";
const ALIAS: &str = "an alias (`*name`)";
const MERGE: &str = "a merge key (`<<`)";
const MERGE_TAG: &str = "a merge key (`!!merge`)";
const TAG: &str = "a tag (`!tag`)";
const EXPLICIT_KEY: &str = "an explicit key (`? `)";
const EXPLICIT_VALUE: &str = "an explicit value (`: ` where a key goes)";
const QUOTED_KEY: &str = "a quoted key";
const MULTI_LINE_QUOTE: &str = "a quoted value that runs over several lines";
const MULTI_LINE_FLOW: &str = "an inline list or mapping that runs over several lines";

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAgents => write!(f, "no `agents:` mapping with an agent in it"),
            Self::Tabs => write!(f, "tabs in its indentation, which YAML forbids"),
            Self::AgentNotBlock(agent) => {
                write!(f, "agent `{agent}` is not written as an indented block")
            }
            Self::ToolsetsNotBlock(agent) => write!(
                f,
                "agent `{agent}` writes `toolsets` inline (`[...]`), not as a list of `- ` items"
            ),
            Self::Construct { line, form } => {
                write!(f, "line {line}: {form}, which this editor does not rewrite")
            }
            Self::Unverified(why) => write!(
                f,
                "the edit could not be confirmed against a YAML reading of the file: {why}"
            ),
        }
    }
}

impl std::error::Error for Problem {}

/// The agent the edit targets: `root`, else the file's first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The agent's key under `agents:`.
    pub agent: String,
    /// It has `harness:` set, so Docker Agent ignores its `toolsets`.
    pub harness: bool,
    /// It already lists AgentDocker's MCP server.
    pub lists_toolset: bool,
}

/// The `toolsets` block to add under an agent, as text the person can
/// paste: the entry at the indent of an agent's keys under `agents:`.
/// `exe` is the absolute path of this binary, quoted as YAML (a JSON
/// string is one).
pub fn docker_agent_toolset(exe: &str) -> String {
    format!("    toolsets:\n{}", toolset_item(exe, 6, "\n"))
}

/// The arguments our entry passes, by which an entry is recognised as
/// ours: `mcp --runtime docker-agent`, or a bare `mcp`.
const ARGS: [&str; 3] = ["mcp", "--runtime", RUNTIME];

fn toolset_item(exe: &str, indent: usize, eol: &str) -> String {
    let pad = " ".repeat(indent);
    let quoted = serde_json::to_string(exe).expect("a string serializes");
    let args = ARGS
        .iter()
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{pad}- type: mcp{eol}{pad}  command: {quoted}{eol}{pad}  args: [{args}]{eol}")
}

/// The YAML with our toolset entry added to its default agent, or `None`
/// when that agent lists it already — confirmed either way against a YAML
/// reading of the file. `marker` is the executable name an entry is
/// recognised by (`agentdocker`), whatever directory it is in.
pub fn with_toolset(yaml: &str, exe: &str, marker: &str) -> Result<Option<String>, Problem> {
    let doc = Document::parse(yaml);
    let agent = doc.target_agent()?;
    let toolsets = doc.toolsets(&agent)?;
    if toolsets.items.iter().any(|item| doc.is_ours(item, marker)) {
        confirm_listed(yaml, &agent.name, marker, true)?;
        return Ok(None);
    }
    let eol = doc.eol();
    let mut out: Vec<String> = doc.lines.iter().map(|l| l.raw.to_owned()).collect();
    match toolsets.key {
        Some(key) => {
            let (indent, at) = match toolsets.items.last() {
                Some(item) => (item.indent, item.end),
                None => (agent.property_indent + 2, key + 1),
            };
            if toolsets.empty_flow {
                // `toolsets: []` becomes `toolsets:` with the list beneath,
                // keeping a comment the line carried.
                let line = &doc.lines[key];
                let (before, rest) = line.text.split_once("[]").expect("an empty flow list");
                let rest = rest.trim_start();
                out[key] = if rest.is_empty() {
                    format!("{}{}", before.trim_end(), line.ending)
                } else {
                    format!("{} {rest}{}", before.trim_end(), line.ending)
                };
            }
            insert(&mut out, at, &toolset_item(exe, indent, eol), eol);
        }
        None => {
            let pad = " ".repeat(agent.property_indent);
            let text = format!(
                "{pad}toolsets:{eol}{}",
                toolset_item(exe, agent.property_indent + 2, eol)
            );
            insert(&mut out, agent.end, &text, eol);
        }
    }
    let edited = out.concat();
    confirm_added(yaml, &edited, &agent.name, exe)?;
    Ok(Some(edited))
}

/// The YAML with our toolset entries taken out of its default agent, or
/// `None` when it lists none — confirmed either way against a YAML reading
/// of the file. A `toolsets` left with no item goes with them, as the
/// entry's own key when `with_toolset` added it; the rest of the file is
/// untouched.
pub fn without_toolset(yaml: &str, marker: &str) -> Result<Option<String>, Problem> {
    let doc = Document::parse(yaml);
    let agent = doc.target_agent()?;
    let toolsets = doc.toolsets(&agent)?;
    let ours: Vec<&Item> = toolsets
        .items
        .iter()
        .filter(|item| doc.is_ours(item, marker))
        .collect();
    if ours.is_empty() {
        confirm_listed(yaml, &agent.name, marker, false)?;
        return Ok(None);
    }
    let mut drop = vec![false; doc.lines.len()];
    for item in &ours {
        drop[item.start..item.end].fill(true);
    }
    if ours.len() == toolsets.items.len()
        && let Some(key) = toolsets.key
    {
        drop[key] = true;
    }
    let kept: String = doc
        .lines
        .iter()
        .enumerate()
        .filter(|(index, _)| !drop[*index])
        .map(|(_, line)| line.raw)
        .collect();
    confirm_removed(yaml, &kept, &agent.name, marker)?;
    Ok(Some(kept))
}

/// The agent the edit targets, with what the caller would need to say
/// about it, confirmed against a YAML reading of the file.
pub fn target(yaml: &str, marker: &str) -> Result<Target, Problem> {
    let doc = Document::parse(yaml);
    let agent = doc.target_agent()?;
    let toolsets = doc.toolsets(&agent)?;
    let lists_toolset = toolsets.items.iter().any(|item| doc.is_ours(item, marker));
    confirm_listed(yaml, &agent.name, marker, lists_toolset)?;
    Ok(Target {
        harness: doc.has_key(&agent, "harness"),
        lists_toolset,
        agent: agent.name,
    })
}

/// Put `text` (whole lines, each ending in `eol`) before line `at`. A last
/// line without a line ending gets one first, so the insertion starts on
/// a line of its own.
fn insert(out: &mut Vec<String>, at: usize, text: &str, eol: &str) {
    if at == out.len()
        && let Some(last) = out.last_mut()
        && !last.ends_with('\n')
    {
        last.push_str(eol);
    }
    out.insert(at, text.to_owned());
}

/// One line of the file: its bytes as they were, and what the editor
/// reads off it.
struct Line<'a> {
    raw: &'a str,
    /// The line without its ending.
    text: &'a str,
    ending: &'a str,
    /// Leading spaces.
    indent: usize,
    /// Blank, or a comment alone — outside a block scalar.
    skip: bool,
    /// Inside a block scalar (`key: |`, `key: >`): text of that value,
    /// whatever it looks like, never a key, an item or a comment.
    scalar: bool,
}

impl Line<'_> {
    /// The line past its indentation, with a trailing comment removed.
    fn content(&self) -> &str {
        strip_comment(&self.text[self.indent..]).trim_end()
    }

    /// `key` when the line is `key:` with nothing (but a comment) after.
    fn block_key(&self) -> Option<&str> {
        let content = self.content();
        let key = content.strip_suffix(':')?;
        (!key.is_empty() && !key.starts_with('-')).then(|| unquote(key.trim()))
    }

    /// `(key, value)` for a `key: value` line.
    fn key_value(&self) -> Option<(&str, &str)> {
        let content = self.content();
        let (key, value) = content.split_once(':')?;
        if key.is_empty() || key.starts_with('-') {
            return None;
        }
        (value.is_empty() || value.starts_with(' ')).then(|| (unquote(key.trim()), value.trim()))
    }
}

/// The agent the edit targets: its lines and where its keys sit.
struct Agent {
    name: String,
    /// The `name:` line.
    key: usize,
    /// One past its last content line.
    end: usize,
    /// The indent of its keys.
    property_indent: usize,
}

/// One `- ` item of a block sequence: its lines, continuation included.
struct Item {
    start: usize,
    end: usize,
    indent: usize,
}

/// The agent's `toolsets`, when it has one.
struct Toolsets {
    /// The `toolsets:` line.
    key: Option<usize>,
    /// Written `toolsets: []`.
    empty_flow: bool,
    items: Vec<Item>,
}

struct Document<'a> {
    lines: Vec<Line<'a>>,
}

impl<'a> Document<'a> {
    fn parse(yaml: &'a str) -> Self {
        let lines = yaml
            .split_inclusive('\n')
            .map(|raw| {
                let text = raw.trim_end_matches(['\n', '\r']);
                let ending = &raw[text.len()..];
                let indent = text.len() - text.trim_start_matches(' ').len();
                let rest = &text[indent..];
                let skip = rest.trim().is_empty() || rest.starts_with('#');
                Line {
                    raw,
                    text,
                    ending,
                    indent,
                    skip,
                    scalar: false,
                }
            })
            .collect::<Vec<_>>();
        let mut doc = Self { lines };
        doc.mark_block_scalars();
        doc
    }

    /// The lines of every block scalar, by its content indentation — the
    /// indentation indicator's, past the node the scalar is the value of
    /// (the key, or a sequence entry's dash), else that of its first line
    /// with anything but spaces on it: from the line after the `|` or `>`
    /// header while lines are that deep, with lines of spaces alone among
    /// them — text when longer than the indentation, empty otherwise — and
    /// the empty lines after the last text when the indicator keeps them
    /// (`|+`). They are the scalar's text, so a `#` among them is not a
    /// comment and a `- ` is not an item.
    fn mark_block_scalars(&mut self) {
        let spaces = |line: &Line| line.text.bytes().all(|byte| byte == b' ');
        let mut index = 0;
        while index < self.lines.len() {
            let line = &self.lines[index];
            let header = if line.skip || line.scalar {
                None
            } else {
                block_scalar_header(line.content())
            };
            let Some(header) = header else {
                index += 1;
                continue;
            };
            let parent = line.indent + header.column;
            let content = match header.indentation {
                Some(indentation) => Some(parent + indentation),
                None => self.lines[index + 1..]
                    .iter()
                    .find(|line| !spaces(line))
                    .map(|line| line.indent)
                    .filter(|&indent| indent > parent),
            };
            let mut next = index + 1;
            let mut last = index;
            while let Some(candidate) = self.lines.get(next) {
                let text = if spaces(candidate) {
                    content.is_some_and(|content| candidate.text.len() > content)
                } else if content.is_some_and(|content| candidate.indent >= content) {
                    true
                } else {
                    break;
                };
                if text {
                    last = next;
                }
                next += 1;
            }
            let end = if header.keep { next } else { last + 1 };
            for line in &mut self.lines[index + 1..end] {
                line.scalar = true;
                line.skip = false;
            }
            index = end.max(index + 1);
        }
    }

    /// The line ending the file uses: the first line's, else `\n`.
    fn eol(&self) -> &'a str {
        self.lines
            .iter()
            .map(|line| line.ending)
            .find(|ending| !ending.is_empty())
            .unwrap_or("\n")
    }

    /// Content lines from `from` while they stay deeper than `indent`:
    /// one past the last of them. Blank and comment lines between are
    /// passed over; trailing ones are left outside.
    fn block_end(&self, from: usize, indent: usize) -> Result<usize, Problem> {
        let mut end = from;
        for (index, line) in self.lines.iter().enumerate().skip(from) {
            if line.skip {
                continue;
            }
            // A block scalar's lines are the value of a key in this block,
            // whatever their own indent: a blank line in one is still in it.
            if line.scalar {
                end = index + 1;
                continue;
            }
            if line.text[line.indent..].starts_with('\t') {
                return Err(Problem::Tabs);
            }
            if line.indent <= indent {
                break;
            }
            end = index + 1;
        }
        Ok(end)
    }

    /// The agent the edit targets, once the file passes the refusals that
    /// hold anywhere in it.
    fn target_agent(&self) -> Result<Agent, Problem> {
        self.refuse_document_wide()?;
        self.default_agent()
    }

    /// Refuse, anywhere in the file outside block-scalar text, what its
    /// lines do not say on their own, or what Docker Agent's YAML reader
    /// and the one that confirms an edit read differently: an anchor, an
    /// alias, a merge key, a tag, an explicit key or value.
    fn refuse_document_wide(&self) -> Result<(), Problem> {
        for (index, line) in self.lines.iter().enumerate() {
            if line.skip || line.scalar {
                continue;
            }
            if let Some(form) = scan(&line.text[line.indent..]).form {
                return Err(Problem::Construct {
                    line: index + 1,
                    form,
                });
            }
        }
        Ok(())
    }

    /// The agent `docker agent run` runs by default: the one named `root`
    /// among the keys under `agents:`, else the first of them. Names and
    /// keys are read as written, so a quoted one — which a YAML reader
    /// decodes, perhaps into another name — is refused: the `agents` key,
    /// the agents' names, and every agent's own keys.
    fn default_agent(&self) -> Result<Agent, Problem> {
        let agents = self
            .lines
            .iter()
            .position(|line| {
                !line.skip && !line.scalar && line.indent == 0 && line.block_key() == Some("agents")
            })
            .ok_or(Problem::NoAgents)?;
        let quoted = |index: usize| {
            if self.lines[index].content().starts_with(['"', '\'']) {
                Err(Problem::Construct {
                    line: index + 1,
                    form: QUOTED_KEY,
                })
            } else {
                Ok(())
            }
        };
        quoted(agents)?;
        let end = self.block_end(agents + 1, 0)?;
        let first = (agents + 1..end)
            .find(|&index| !self.lines[index].skip)
            .ok_or(Problem::NoAgents)?;
        let indent = self.lines[first].indent;
        let keys: Vec<usize> = (first..end)
            .filter(|&index| {
                let line = &self.lines[index];
                !line.skip && !line.scalar && line.indent == indent
            })
            .collect();
        for &index in &keys {
            quoted(index)?;
            let lines: Vec<usize> = (index + 1..self.block_end(index + 1, indent)?)
                .filter(|&own| !self.lines[own].skip && !self.lines[own].scalar)
                .collect();
            if let Some(&property) = lines.first() {
                let property_indent = self.lines[property].indent;
                for &own in &lines {
                    if self.lines[own].indent == property_indent {
                        quoted(own)?;
                    }
                }
            }
        }
        let key = keys
            .iter()
            .copied()
            .find(|&index| {
                self.lines[index]
                    .key_value()
                    .is_some_and(|(name, _)| name == "root")
            })
            .unwrap_or(first);
        let line = &self.lines[key];
        let name = line
            .block_key()
            .ok_or_else(|| {
                Problem::AgentNotBlock(
                    line.key_value()
                        .map(|(key, _)| key.to_owned())
                        .unwrap_or_else(|| line.content().to_owned()),
                )
            })?
            .to_owned();
        let end = self.block_end(key + 1, line.indent)?;
        let property_indent = (key + 1..end)
            .find(|&index| !self.lines[index].skip)
            .map(|index| self.lines[index].indent)
            .unwrap_or(line.indent + 2);
        Ok(Agent {
            name,
            key,
            end,
            property_indent,
        })
    }

    /// Refuse, in the agent the edit goes into, what these lines would
    /// misread: a quoted value or an inline list or mapping that runs over
    /// several lines.
    fn refuse_in_agent(&self, agent: &Agent) -> Result<(), Problem> {
        for (index, line) in self
            .lines
            .iter()
            .enumerate()
            .take(agent.end)
            .skip(agent.key)
        {
            if line.skip || line.scalar {
                continue;
            }
            let found = scan(&line.text[line.indent..]);
            let form = if found.open_quote {
                MULTI_LINE_QUOTE
            } else if found.open_flow {
                MULTI_LINE_FLOW
            } else {
                continue;
            };
            return Err(Problem::Construct {
                line: index + 1,
                form,
            });
        }
        Ok(())
    }

    /// The agent's own keys: the content lines at its property indent.
    fn properties(&self, agent: &Agent) -> impl Iterator<Item = usize> + '_ {
        let indent = agent.property_indent;
        (agent.key + 1..agent.end).filter(move |&index| {
            let line = &self.lines[index];
            !line.skip && !line.scalar && line.indent == indent
        })
    }

    fn has_key(&self, agent: &Agent, key: &str) -> bool {
        self.properties(agent)
            .any(|index| self.lines[index].key_value().is_some_and(|(k, _)| k == key))
    }

    fn toolsets(&self, agent: &Agent) -> Result<Toolsets, Problem> {
        self.refuse_in_agent(agent)?;
        let Some(key) = self.properties(agent).find(|&index| {
            self.lines[index]
                .key_value()
                .is_some_and(|(k, _)| k == "toolsets")
        }) else {
            return Ok(Toolsets {
                key: None,
                empty_flow: false,
                items: Vec::new(),
            });
        };
        let (_, value) = self.lines[key].key_value().expect("found by key");
        if value == "[]" {
            return Ok(Toolsets {
                key: Some(key),
                empty_flow: true,
                items: Vec::new(),
            });
        }
        if !value.is_empty() {
            return Err(Problem::ToolsetsNotBlock(agent.name.clone()));
        }
        // The items: `- ` lines at one indent, at or past the key's own
        // (YAML allows both), each with the deeper lines that follow it.
        let mut items = Vec::new();
        let mut index = key + 1;
        let mut item_indent = None;
        while index < agent.end {
            let line = &self.lines[index];
            if line.skip {
                index += 1;
                continue;
            }
            let is_item = !line.scalar
                && line.text[line.indent..].starts_with('-')
                && line.indent >= agent.property_indent
                && item_indent.is_none_or(|indent| indent == line.indent);
            if !is_item {
                break;
            }
            item_indent = Some(line.indent);
            let end = self.block_end(index + 1, line.indent)?;
            items.push(Item {
                start: index,
                end,
                indent: line.indent,
            });
            index = end;
        }
        Ok(Toolsets {
            key: Some(key),
            empty_flow: false,
            items,
        })
    }

    /// The item's `key: value` pairs at its own indent, the first one on
    /// the `- ` line itself; a block-sequence value (`args:` with `- `
    /// lines beneath) is joined into a flow one so that one reading
    /// serves both.
    fn item_pairs(&self, item: &Item) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = Vec::new();
        let inner = item.indent + 2;
        for index in item.start..item.end {
            let line = &self.lines[index];
            if line.skip || line.scalar {
                continue;
            }
            let text = if index == item.start {
                strip_comment(line.text[line.indent + 1..].trim_start()).trim_end()
            } else if line.indent == inner {
                line.content()
            } else if line.indent > inner && line.text[line.indent..].starts_with('-') {
                // A block-sequence element of the pair before it.
                if let Some((_, value)) = pairs.last_mut() {
                    let element = unquote(line.content()[1..].trim()).to_owned();
                    if value.is_empty() {
                        value.push_str(&element);
                    } else {
                        value.push_str(", ");
                        value.push_str(&element);
                    }
                }
                continue;
            } else {
                continue;
            };
            if let Some((key, value)) = text.split_once(':')
                && !key.is_empty()
                && (value.is_empty() || value.starts_with(' '))
            {
                pairs.push((unquote(key.trim()).to_owned(), value.trim().to_owned()));
            }
        }
        pairs
    }

    /// Whether an item runs AgentDocker's MCP server: `type: mcp`, a
    /// `command` whose file name is the marker, and the arguments the
    /// setup writes. A `ref` or `remote` toolset, or a command of another
    /// name, is somebody else's whatever it mentions.
    fn is_ours(&self, item: &Item, marker: &str) -> bool {
        let pairs = self.item_pairs(item);
        let get = |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        let command = get("command").map(unquote).unwrap_or("");
        let file_name = command.rsplit(['/', '\\']).next().unwrap_or(command);
        let args: Vec<&str> = get("args")
            .map(|args| {
                args.trim_start_matches('[')
                    .trim_end_matches(']')
                    .split(',')
                    .map(|a| unquote(a.trim()))
                    .filter(|a| !a.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        get("type").map(unquote) == Some("mcp")
            && (file_name == marker || file_name == format!("{marker}.exe"))
            && (args == ["mcp"] || args == ARGS)
    }
}

/// A block scalar's header, as [`block_scalar_header`] reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    /// The column, past the line's indent, of the node the scalar is the
    /// value of — the key, or a sequence entry's dash — which its lines
    /// are deeper than.
    column: usize,
    /// `+` chomping: the empty lines after the text are the value's.
    keep: bool,
    /// An explicit indentation indicator (`|2`): the content indentation,
    /// past the node's column.
    indentation: Option<usize>,
}

/// A block scalar's header: a line's content (past its indent, comment
/// removed) that ends in a `|` or `>` indicator — with chomping and
/// indentation indicators in either order — as the value of a key
/// (`key: |`, `- key: >-`) or of a sequence entry (`- |`), an anchor or a
/// tag allowed before it.
fn block_scalar_header(content: &str) -> Option<Header> {
    // Sequence entry indicators in front: `- `, `- - `.
    let mut column = 0;
    let mut dash = None;
    let mut rest = content;
    while let Some(after) = rest.strip_prefix('-') {
        if !after.is_empty() && !after.starts_with(' ') {
            break;
        }
        dash = Some(column);
        let trimmed = after.trim_start();
        column += rest.len() - trimmed.len();
        rest = trimmed;
    }
    let mut words = rest.split_whitespace().rev();
    let indicator = words.next()?;
    let mut before = words.skip_while(|word| word.starts_with('&') || word.starts_with('!'));
    let parent = match before.next() {
        // `key: |`: the value of the key, which starts the rest.
        Some(key) if key.ends_with(':') => column,
        Some(_) => return None,
        // `- |`: the value of the entry.
        None => dash?,
    };
    let mut chars = indicator.chars();
    if !matches!(chars.next(), Some('|' | '>')) {
        return None;
    }
    let flags = chars.as_str();
    let chomping = flags.chars().filter(|c| matches!(c, '-' | '+')).count();
    let digits: Vec<usize> = flags
        .chars()
        .filter_map(|c| c.to_digit(10))
        .map(|digit| digit as usize)
        .collect();
    if flags.chars().all(|c| matches!(c, '-' | '+' | '1'..='9'))
        && chomping <= 1
        && digits.len() <= 1
    {
        Some(Header {
            column: parent,
            keep: flags.contains('+'),
            indentation: digits.first().copied(),
        })
    } else {
        None
    }
}

/// What a line's content holds outside its quoted scalars and its
/// comment, as YAML's indicators read it. A quote opens a quoted scalar,
/// and `&`, `*`, `!`, `?` and `:` act, only where a node may start — at
/// the content's start, after `- `, `? `, `: `, an anchor or a tag, or
/// after `[`, `{` or `,` in an inline collection; `#` opens a comment only
/// at the start or after whitespace. Inside a plain scalar they are text:
/// `it's`, `a *b* & c`.
#[derive(Debug, Default, PartialEq, Eq)]
struct Scan {
    /// Where the comment starts; the content's length when there is none.
    comment: usize,
    /// The first indicator the editor refuses wherever it is: an anchor,
    /// an alias, a merge key, a tag, an explicit key or value.
    form: Option<&'static str>,
    /// A quoted scalar still open at the end.
    open_quote: bool,
    /// An inline list or mapping still open at the end, or closed there
    /// more often than opened.
    open_flow: bool,
}

fn scan(content: &str) -> Scan {
    let mut found = Scan {
        comment: content.len(),
        ..Scan::default()
    };
    let mut chars = content.char_indices().peekable();
    let mut quote: Option<char> = None;
    let mut start = true;
    let mut depth = 0i32;
    let mut previous = ' ';
    while let Some((at, c)) = chars.next() {
        let next = chars.peek().map(|&(_, next)| next);
        let spaced = next.is_none_or(char::is_whitespace);
        match quote {
            Some('"') => match c {
                // An escape takes the character after it.
                '\\' => {
                    chars.next();
                }
                '"' => quote = None,
                _ => {}
            },
            Some(_) => {
                // A single-quoted scalar doubles a quote inside it.
                if c == '\'' {
                    if next == Some('\'') {
                        chars.next();
                    } else {
                        quote = None;
                    }
                }
            }
            None => match c {
                '#' if previous.is_whitespace() => {
                    found.comment = at;
                    break;
                }
                c if c.is_whitespace() => {}
                '"' | '\'' if start => {
                    quote = Some(c);
                    start = false;
                }
                '&' | '*' if start && !spaced => {
                    found.form = found.form.or(Some(if c == '&' { ANCHOR } else { ALIAS }));
                    // The name runs to the next space or inline indicator.
                    while chars
                        .next_if(|&(_, n)| {
                            !n.is_whitespace() && !matches!(n, ',' | '[' | ']' | '{' | '}')
                        })
                        .is_some()
                    {}
                    // An anchor names the node after it; an alias is one.
                    start = c == '&';
                }
                '!' if start => {
                    let mut tag = String::from('!');
                    while let Some((_, n)) = chars.next_if(|&(_, n)| !n.is_whitespace()) {
                        tag.push(n);
                    }
                    found.form =
                        found
                            .form
                            .or(Some(if tag == "!!merge" { MERGE_TAG } else { TAG }));
                }
                '?' if start && spaced => found.form = found.form.or(Some(EXPLICIT_KEY)),
                ':' if spaced => {
                    if start {
                        found.form = found.form.or(Some(EXPLICIT_VALUE));
                    }
                    start = true;
                }
                '-' if start && spaced => {}
                '[' | '{' if start || depth > 0 => {
                    depth += 1;
                    start = true;
                }
                ']' | '}' if depth > 0 => {
                    depth -= 1;
                    start = false;
                }
                ',' if depth > 0 => start = true,
                '<' if next == Some('<') => {
                    found.form = found.form.or(Some(MERGE));
                    start = false;
                }
                _ => start = false,
            },
        }
        previous = c;
    }
    found.open_quote = quote.is_some();
    found.open_flow = depth != 0;
    found
}

/// The text before its comment, as [`scan`] reads it.
fn strip_comment(text: &str) -> &str {
    &text[..scan(text).comment]
}

/// A quoted scalar without its quotes. YAML double quotes escape as JSON
/// does for everything setup writes; a single-quoted scalar doubles its
/// quotes, which this leaves as they are, since a path with one is not
/// compared here. Anything else is returned as written.
fn unquote(value: &str) -> &str {
    let inner = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')));
    inner.unwrap_or(value)
}

// The runtime check: every edit read again as YAML before it is returned.

fn key(name: &str) -> Yaml {
    Yaml::String(name.to_owned())
}

/// The one document a YAML parser reads in the text. A text it cannot
/// read — a key twice in one mapping among the reasons, which it names
/// with the line — or one with several documents is not one an edit can
/// be confirmed against.
fn read(text: &str, which: &str) -> Result<Yaml, Problem> {
    let mut documents = YamlLoader::load_from_str(text)
        .map_err(|error| Problem::Unverified(format!("{which} does not read as YAML: {error}")))?;
    if documents.len() > 1 {
        return Err(Problem::Unverified(format!(
            "{which} holds {} YAML documents",
            documents.len()
        )));
    }
    Ok(documents.pop().unwrap_or(Yaml::Null))
}

/// The default agent as a parsed document names it: `root` among the
/// agents, else the first, by its decoded key.
fn read_default(document: &Yaml) -> Result<String, Problem> {
    let agents = document["agents"].as_hash().ok_or_else(|| {
        Problem::Unverified("the YAML reading has no `agents` mapping".to_owned())
    })?;
    if agents.contains_key(&key("root")) {
        return Ok("root".to_owned());
    }
    match agents.keys().next() {
        Some(Yaml::String(name)) => Ok(name.clone()),
        Some(other) => Err(Problem::Unverified(format!(
            "the YAML reading's first agent is not named by a string: {other:?}"
        ))),
        None => Err(Problem::NoAgents),
    }
}

/// The file as a YAML parser reads it, the agent the lines chose confirmed
/// as the one that reading names as the default.
fn read_confirmed(original: &str, agent: &str) -> Result<Yaml, Problem> {
    let document = read(original, "the file")?;
    let default = read_default(&document)?;
    if default != agent {
        return Err(Problem::Unverified(format!(
            "the YAML reading's default agent is `{default}`, not `{agent}`"
        )));
    }
    Ok(document)
}

/// Our entry as a parser reads the lines `with_toolset` writes.
fn entry(exe: &str) -> Yaml {
    let mut entry = Hash::new();
    entry.insert(key("type"), key("mcp"));
    entry.insert(key("command"), key(exe));
    entry.insert(
        key("args"),
        Yaml::Array(ARGS.iter().map(|arg| key(arg)).collect()),
    );
    Yaml::Hash(entry)
}

/// Whether a parsed toolset runs our server, as [`Document::is_ours`]
/// reads it off its lines.
fn runs_us(item: &Yaml, marker: &str) -> bool {
    let args: Option<Vec<&str>> = item["args"]
        .as_vec()
        .and_then(|args| args.iter().map(Yaml::as_str).collect());
    let file = item["command"]
        .as_str()
        .map(|command| command.rsplit(['/', '\\']).next().unwrap_or(command));
    item["type"].as_str() == Some("mcp")
        && file.is_some_and(|file| file == marker || file.strip_suffix(".exe") == Some(marker))
        && args.is_some_and(|args| args == ["mcp"] || args == ARGS)
}

fn lists_us(document: &Yaml, agent: &str, marker: &str) -> bool {
    document["agents"][agent]["toolsets"]
        .as_vec()
        .is_some_and(|items| items.iter().any(|item| runs_us(item, marker)))
}

/// The document with `edit` made to an agent's mapping: one made for it
/// when the agent was empty, read as empty again when the edit leaves it
/// so.
fn with_agent(document: &Yaml, agent: &str, edit: impl FnOnce(&mut Hash)) -> Yaml {
    let mut document = document.clone();
    let found = document
        .as_mut_hash()
        .and_then(|top| top.get_mut(&key("agents")))
        .and_then(Yaml::as_mut_hash)
        .and_then(|agents| agents.get_mut(&key(agent)));
    if let Some(value) = found {
        if value.is_null() {
            *value = Yaml::Hash(Hash::new());
        }
        if let Yaml::Hash(mapping) = value {
            edit(mapping);
            if mapping.is_empty() {
                *value = Yaml::Null;
            }
        }
    }
    document
}

/// What an addition must read as: the original with our entry added,
/// last, to the agent's toolsets.
fn with_entry(document: &Yaml, agent: &str, exe: &str) -> Yaml {
    with_agent(document, agent, |mapping| {
        match mapping.get_mut(&key("toolsets")) {
            Some(Yaml::Array(items)) => items.push(entry(exe)),
            Some(value) if value.is_null() => *value = Yaml::Array(vec![entry(exe)]),
            Some(_) => {}
            None => {
                mapping.insert(key("toolsets"), Yaml::Array(vec![entry(exe)]));
            }
        }
    })
}

/// What a removal must read as: the original without our entries in the
/// agent's toolsets, a list left empty going with them.
fn without_entries(document: &Yaml, agent: &str, marker: &str) -> Yaml {
    with_agent(document, agent, |mapping| {
        if let Some(Yaml::Array(items)) = mapping.get_mut(&key("toolsets")) {
            items.retain(|item| !runs_us(item, marker));
            if items.is_empty() {
                mapping.remove(&key("toolsets"));
            }
        }
    })
}

/// Confirm an addition: the edited file reads as the original with only
/// our entry added to the default agent's toolsets.
fn confirm_added(original: &str, edited: &str, agent: &str, exe: &str) -> Result<(), Problem> {
    let before = read_confirmed(original, agent)?;
    let after = read(edited, "the edited file")?;
    if after == with_entry(&before, agent, exe) {
        Ok(())
    } else {
        Err(Problem::Unverified(format!(
            "the edited file reads as more than our toolset added to agent `{agent}`"
        )))
    }
}

/// Confirm a removal: the edited file reads as the original with only our
/// entries taken out of the default agent's toolsets.
fn confirm_removed(original: &str, edited: &str, agent: &str, marker: &str) -> Result<(), Problem> {
    let before = read_confirmed(original, agent)?;
    let after = read(edited, "the edited file")?;
    if lists_us(&before, agent, marker) && after == without_entries(&before, agent, marker) {
        Ok(())
    } else {
        Err(Problem::Unverified(format!(
            "the edited file reads as more than our toolset taken out of agent `{agent}`"
        )))
    }
}

/// Confirm what the lines say of the default agent without an edit:
/// whether it lists our toolset.
fn confirm_listed(original: &str, agent: &str, marker: &str, listed: bool) -> Result<(), Problem> {
    let before = read_confirmed(original, agent)?;
    if lists_us(&before, agent, marker) == listed {
        Ok(())
    } else if listed {
        Err(Problem::Unverified(format!(
            "these lines find our toolset in agent `{agent}`, the YAML reading does not"
        )))
    } else {
        Err(Problem::Unverified(format!(
            "the YAML reading finds our toolset in agent `{agent}`, these lines do not"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/opt/agentdocker/bin/agentdocker";

    const PLAIN: &str =
        "agents:\n  root:\n    model: openai/gpt-5-mini\n    description: A helpful assistant\n";

    // Every edit `with_toolset` and `without_toolset` make is confirmed
    // before it is returned against a YAML reading of the file (see
    // `confirm_added`): the whole fixture corpus runs through them, and
    // `add` checks the bytes of each round trip besides.

    /// Add our entry, and take it out again: the removal gives the original
    /// bytes back (but for the line ending a last line without one gets,
    /// and a `toolsets: []` opened into a list, whose key then goes).
    #[track_caller]
    fn add(yaml: &str) -> Option<String> {
        let after = with_toolset(yaml, EXE, "agentdocker")
            .unwrap_or_else(|problem| panic!("{problem}:\n{yaml}"));
        if let Some(edited) = &after {
            let back = remove(edited).expect("ours comes out again");
            if !yaml.contains("toolsets: []") {
                assert!(
                    back == yaml
                        || back
                            .strip_prefix(yaml)
                            .is_some_and(|rest| rest == "\n" || rest == "\r\n"),
                    "the removal is the reverse:\n{yaml}\n---\n{back}"
                );
            }
        }
        after
    }

    #[track_caller]
    fn remove(yaml: &str) -> Option<String> {
        without_toolset(yaml, "agentdocker").unwrap_or_else(|problem| panic!("{problem}:\n{yaml}"))
    }

    /// The default agent's instruction, as the parser reads it.
    fn instruction(yaml: &str) -> String {
        let document = read(yaml, "the fixture").unwrap();
        let name = read_default(&document).unwrap();
        document["agents"][name.as_str()]["instruction"]
            .as_str()
            .expect("an instruction")
            .to_owned()
    }

    /// The block setup prints is the entry at an agent's indent, quoted as
    /// YAML, with the arguments the daemon recognises, and reads as the
    /// entry the check expects.
    #[test]
    fn the_printed_toolset_is_the_entry_at_an_agents_indent() {
        let text = docker_agent_toolset("/Apps/Agent Docker/agentdocker");
        assert_eq!(
            text,
            "    toolsets:\n      - type: mcp\n        command: \"/Apps/Agent Docker/agentdocker\"\n        args: [\"mcp\", \"--runtime\", \"docker-agent\"]\n"
        );
        let document = read(
            &format!("agents:\n  root:\n{}", docker_agent_toolset(EXE)),
            "the fixture",
        )
        .unwrap();
        assert_eq!(document["agents"]["root"]["toolsets"][0], entry(EXE));
    }

    /// An agent without `toolsets` gets the key and our entry after its
    /// last line; the rest of the file is byte for byte as it was, and the
    /// edit is recognised afterwards, so a second setup adds nothing.
    #[test]
    fn an_agent_without_toolsets_gets_the_key_and_the_entry() {
        let after = add(PLAIN).unwrap();
        assert_eq!(
            after,
            format!(
                "{PLAIN}    toolsets:\n      - type: mcp\n        command: \"{EXE}\"\n        args: [\"mcp\", \"--runtime\", \"docker-agent\"]\n"
            )
        );
        assert_eq!(add(&after), None);
        assert_eq!(
            target(&after, "agentdocker").unwrap(),
            Target {
                agent: "root".into(),
                harness: false,
                lists_toolset: true,
            }
        );
        assert_eq!(
            remove(&after).unwrap(),
            PLAIN,
            "the removal is the exact reverse"
        );
        assert_eq!(remove(PLAIN), None);
        // An agent with no keys yet gets them too.
        let empty = "agents:\n  root:\n  helper:\n    model: fast\n";
        assert!(
            add(empty)
                .unwrap()
                .starts_with("agents:\n  root:\n    toolsets:\n")
        );
    }

    /// An existing list keeps its items, comments and the person's own
    /// indent; ours goes last, and only ours goes at removal.
    #[test]
    fn an_existing_list_keeps_its_items_and_takes_ours_last() {
        let yaml = "# team\nmodels:\n  fast: openai/gpt-5-mini\n\nagents:\n  root:\n    model: fast\n    toolsets:\n      # search the web\n      - type: mcp\n        ref: docker:duckduckgo\n      - type: filesystem\n\n    sub_agents: [writer]\n\n  writer:\n    model: fast\n";
        let after = add(yaml).unwrap();
        let expected = yaml.replace(
            "      - type: filesystem\n",
            &format!(
                "      - type: filesystem\n      - type: mcp\n        command: \"{EXE}\"\n        args: [\"mcp\", \"--runtime\", \"docker-agent\"]\n"
            ),
        );
        assert_eq!(after, expected);
        assert_eq!(remove(&after).unwrap(), yaml);
        // Items at the key's own indent are a list too.
        let flush = "agents:\n  root:\n    model: fast\n    toolsets:\n    - type: filesystem\n    instruction: go\n";
        let after = add(flush).unwrap();
        assert_eq!(
            after,
            format!(
                "agents:\n  root:\n    model: fast\n    toolsets:\n    - type: filesystem\n    - type: mcp\n      command: \"{EXE}\"\n      args: [\"mcp\", \"--runtime\", \"docker-agent\"]\n    instruction: go\n"
            )
        );
        assert_eq!(remove(&after).unwrap(), flush);
    }

    /// `toolsets: []` opens into a list; taking the last item out takes the
    /// key with it. An inline list with items is refused, never rewritten.
    #[test]
    fn an_empty_inline_list_opens_and_a_full_one_is_refused() {
        let yaml = "agents:\n  root:\n    toolsets: []  # none yet\n    model: fast\n";
        let after = add(yaml).unwrap();
        assert_eq!(
            after,
            format!(
                "agents:\n  root:\n    toolsets: # none yet\n      - type: mcp\n        command: \"{EXE}\"\n        args: [\"mcp\", \"--runtime\", \"docker-agent\"]\n    model: fast\n"
            )
        );
        assert_eq!(
            remove(&after).unwrap(),
            "agents:\n  root:\n    model: fast\n"
        );
        let inline = "agents:\n  root:\n    toolsets: [{type: filesystem}]\n";
        assert_eq!(
            with_toolset(inline, EXE, "agentdocker"),
            Err(Problem::ToolsetsNotBlock("root".into()))
        );
        assert_eq!(
            with_toolset("agents:\n  root: {model: fast}\n", EXE, "agentdocker"),
            Err(Problem::AgentNotBlock("root".into()))
        );
        for empty in ["", "models:\n  fast: x\n", "agents:\n", "agents: {}\n"] {
            assert_eq!(
                with_toolset(empty, EXE, "agentdocker"),
                Err(Problem::NoAgents),
                "{empty:?}"
            );
        }
        assert_eq!(
            with_toolset("agents:\n\troot:\n\t\tmodel: x\n", EXE, "agentdocker"),
            Err(Problem::Tabs)
        );
    }

    /// Ours is known by what it runs, in either quoting and either list
    /// form, not by the word `agentdocker` anywhere in an item; another
    /// agent's entry is not this agent's. The parser agrees each time.
    #[test]
    fn our_entry_is_known_by_what_it_runs() {
        let listed = |item: &str| {
            let yaml =
                format!("agents:\n  root:\n    toolsets:\n{item}  helper:\n    model: fast\n");
            let listed = target(&yaml, "agentdocker").unwrap().lists_toolset;
            assert_eq!(add(&yaml).is_none(), listed, "{yaml}");
            listed
        };
        assert!(listed(
            "      - type: mcp\n        command: /usr/local/bin/agentdocker\n        args: [mcp, --runtime, docker-agent]\n"
        ));
        assert!(listed(
            "      - type: mcp\n        command: 'C:\\Tools\\agentdocker.exe'\n        args:\n          - mcp\n"
        ));
        assert!(listed(
            "      - command: \"/opt/agentdocker\"  # ours\n        type: \"mcp\"\n        args: [\"mcp\", \"--runtime\", \"docker-agent\"]\n"
        ));
        assert!(!listed(
            "      - type: mcp\n        command: /opt/agentdocker\n        args: [\"hook\", \"docker-agent\"]\n"
        ));
        assert!(!listed(
            "      - type: shell\n        command: agentdocker\n        args: [\"mcp\"]\n"
        ));
        assert!(!listed(
            "      - type: mcp\n        command: /opt/wrapper\n        args: [\"agentdocker\", \"mcp\"]\n"
        ));
        assert!(!listed(
            "      - type: mcp\n        ref: docker:agentdocker\n"
        ));
        // Another agent's entry is not the default agent's.
        let other = format!(
            "agents:\n  root:\n    model: fast\n  helper:\n{}",
            docker_agent_toolset(EXE)
        );
        assert!(!target(&other, "agentdocker").unwrap().lists_toolset);
        assert!(add(&other).is_some());
    }

    /// The edit goes where `docker agent run` looks: the agent named
    /// `root`, wherever it is in the file, else the first.
    #[test]
    fn the_default_agent_is_root_else_the_first() {
        let later = "agents:\n  helper:\n    model: fast\n  root:\n    model: fast\n  writer:\n    model: fast\n";
        assert_eq!(target(later, "agentdocker").unwrap().agent, "root");
        assert_eq!(
            add(later).unwrap(),
            format!(
                "agents:\n  helper:\n    model: fast\n  root:\n    model: fast\n    toolsets:\n      - type: mcp\n        command: \"{EXE}\"\n        args: [\"mcp\", \"--runtime\", \"docker-agent\"]\n  writer:\n    model: fast\n"
            )
        );
        // A `root` that is no block is refused, not passed over.
        assert_eq!(
            with_toolset(
                "agents:\n  helper:\n    model: fast\n  root: {model: fast}\n",
                EXE,
                "agentdocker"
            ),
            Err(Problem::AgentNotBlock("root".into()))
        );
        // A key called root deeper down is not an agent.
        let nested = "agents:\n  helper:\n    model: fast\n    root:\n      x: 1\n";
        assert_eq!(target(nested, "agentdocker").unwrap().agent, "helper");
        add(nested);
    }

    /// A block scalar is one value whatever its lines look like: a `#`
    /// line inside an instruction is text, not a comment, a line of spaces
    /// longer than its content indentation is text too, and `|+` keeps its
    /// empty lines; the entry goes after the whole scalar, and the parser
    /// reads the instruction the same before and after.
    #[test]
    fn a_block_scalar_keeps_its_text_whole() {
        let yaml = "agents:\n  root:\n    model: fast\n    instruction: |\n      Follow the rules.\n      # Keep all filenames private\n      - never a list\n  helper:\n    model: fast\n";
        let after = add(yaml).unwrap();
        assert_eq!(
            after,
            format!(
                "agents:\n  root:\n    model: fast\n    instruction: |\n      Follow the rules.\n      # Keep all filenames private\n      - never a list\n    toolsets:\n      - type: mcp\n        command: \"{EXE}\"\n        args: [\"mcp\", \"--runtime\", \"docker-agent\"]\n  helper:\n    model: fast\n"
            )
        );
        assert_eq!(
            instruction(yaml),
            "Follow the rules.\n# Keep all filenames private\n- never a list\n"
        );
        assert_eq!(instruction(&after), instruction(yaml));
        // A scalar of `#` lines alone is still the whole value.
        let hashes = "agents:\n  root:\n    instruction: >-\n      # one\n      # two\n";
        let after = add(hashes).unwrap();
        assert!(after.starts_with(
            "agents:\n  root:\n    instruction: >-\n      # one\n      # two\n    toolsets:\n"
        ));
        assert_eq!(instruction(hashes), "# one # two");
        // `|+` keeps the empty lines after the text: the entry goes after
        // them, and empty lines inside a scalar stay inside.
        let keep = "agents:\n  root:\n    instruction: |+\n      First.\n\n      Last.\n\n\n  helper:\n    model: fast\n";
        let after = add(keep).unwrap();
        assert!(after.starts_with(
            "agents:\n  root:\n    instruction: |+\n      First.\n\n      Last.\n\n\n    toolsets:\n"
        ));
        assert_eq!(instruction(keep), "First.\n\nLast.\n\n\n");
        // A line of spaces past the content indentation is text, kept or
        // not; one no longer than it is an empty line.
        for (yaml, value) in [
            (
                "agents:\n  root:\n    instruction: |\n      text\n        \n  helper:\n    model: fast\n",
                "text\n  \n",
            ),
            (
                "agents:\n  root:\n    instruction: |\n      text\n         \n    model: fast\n",
                "text\n   \n",
            ),
            (
                "agents:\n  root:\n    instruction: |+\n      text\n        \n      \n  helper:\n    model: fast\n",
                "text\n  \n\n",
            ),
            (
                "agents:\n  root:\n    instruction: |\n      text\n      \n  helper:\n    model: fast\n",
                "text\n",
            ),
        ] {
            assert_eq!(instruction(yaml), value, "{yaml}");
            assert_eq!(instruction(&add(yaml).unwrap()), value, "{yaml}");
        }
        // An explicit indentation indicator sets the content indentation,
        // past the key: a line less deep than the first is still text.
        let explicit = "agents:\n  root:\n    model: fast\n    instruction: |1\n      deep\n     shallow\n  helper:\n    model: fast\n";
        assert_eq!(instruction(explicit), " deep\nshallow\n");
        let after = add(explicit).unwrap();
        assert!(after.contains("     shallow\n    toolsets:\n"), "{after}");
        // A scalar inside an item is that item's, and the item's keys are
        // still read around it, on its own line or on the dash's.
        for item in [
            "agents:\n  root:\n    toolsets:\n      - type: mcp\n        instruction: |\n          # use it\n          - always\n        command: /opt/agentdocker\n        args: [mcp]\n",
            "agents:\n  root:\n    toolsets:\n      - instruction: |+\n          # use it\n\n        type: mcp\n        command: /opt/agentdocker\n        args: [mcp]\n",
        ] {
            assert!(target(item, "agentdocker").unwrap().lists_toolset, "{item}");
            assert_eq!(remove(item).unwrap(), "agents:\n  root:\n", "{item}");
        }
        for (content, header) in [
            ("instruction: |", Some((0, false, None))),
            ("instruction: >-", Some((0, false, None))),
            ("instruction: |+", Some((0, true, None))),
            ("instruction: |2+", Some((0, true, Some(2)))),
            ("instruction: >-1", Some((0, false, Some(1)))),
            ("instruction: &doc !!str |", Some((0, false, None))),
            ("\"a:b\": >-", Some((0, false, None))),
            ("- |", Some((0, false, None))),
            ("- note: >+", Some((2, true, None))),
            ("-   - note: |", Some((6, false, None))),
            ("note: a |", None),
            ("instruction: a | b", None),
            ("instruction: |x", None),
            ("instruction: |--", None),
            ("instruction: |12", None),
            ("command: /bin/x", None),
            ("--flag: |x", None),
            ("|", None),
            ("-", None),
        ] {
            assert_eq!(
                block_scalar_header(content).map(|h| (h.column, h.keep, h.indentation)),
                header,
                "{content}"
            );
        }
    }

    /// What the lines do not say on their own, or what Docker Agent's YAML
    /// reader and the one that confirms an edit read differently, is
    /// refused wherever it is in the file, with the line and the form: an
    /// anchor (an anchored item another agent aliases, which a removal
    /// would leave naming nothing), an alias, a merge key (on an agent, or
    /// on the `agents` mapping itself), a tag, an explicit key; so is a
    /// quoted key among the agents or an agent's own keys (an escaped
    /// `"r\x6fot"` is `root`, an escaped `"tool\x73ets"` is `toolsets`),
    /// and, in the agent edited, a quoted value or an inline collection
    /// over several lines. Text that only looks like them is text.
    #[test]
    fn what_the_editor_cannot_read_with_certainty_is_refused() {
        for (yaml, line, form) in [
            (
                "agents:\n  root:\n    toolsets:\n      - &shared\n        type: filesystem\n  helper:\n    toolsets:\n      - *shared\n",
                4,
                ANCHOR,
            ),
            (
                "agents:\n  helper:\n    toolsets:\n      - &shared\n        type: filesystem\n  root:\n    toolsets:\n      - *shared\n",
                4,
                ANCHOR,
            ),
            (
                "defaults: &defaults\n  toolsets:\n    - type: filesystem\nagents:\n  root:\n    ? <<\n    : *defaults\n    model: fast\n",
                1,
                ANCHOR,
            ),
            (
                "defaults: &defaults\n  model: fast\nagents:\n  root:\n    model: fast\n  helper:\n    <<: *defaults\n",
                1,
                ANCHOR,
            ),
            (
                "agents:\n  root:\n    model: fast\n    <<: [*a, *b]\n",
                4,
                MERGE,
            ),
            ("agents:\n  root:\n    toolsets: *shared\n", 3, ALIAS),
            (
                "agents:\n  root:\n    toolsets:\n      - type: script\n        env: {<<: {A: b}}\n",
                5,
                MERGE,
            ),
            (
                "agents:\n  root:\n    !!merge <<: {model: fast}\n",
                3,
                MERGE_TAG,
            ),
            // A merge on the agents mapping itself, which may bring `root`.
            (
                "defaults: &defaults\n  root:\n    model: fast\nagents:\n  <<: *defaults\n  helper:\n    model: fast\n",
                1,
                ANCHOR,
            ),
            (
                "agents:\n  <<: {root: {model: fast}}\n  helper:\n    model: fast\n",
                2,
                MERGE,
            ),
            // Tags and explicit keys, anywhere.
            (
                "models:\n  fast: !custom openai/gpt-5-mini\nagents:\n  root:\n    model: fast\n",
                2,
                TAG,
            ),
            (
                "agents:\n  helper:\n    model: fast\n  !!str root:\n    model: fast\n",
                4,
                TAG,
            ),
            (
                "agents:\n  root:\n    ? model\n    : fast\n",
                3,
                EXPLICIT_KEY,
            ),
            (
                "agents:\n  root:\n    model: fast\n    ? [a, b]\n    : c\n",
                4,
                EXPLICIT_KEY,
            ),
            (
                "agents:\n  helper:\n    model: fast\n  ? root\n  : {model: fast}\n",
                4,
                EXPLICIT_KEY,
            ),
            ("agents:\n  &a root:\n    model: fast\n", 2, ANCHOR),
            // Quoted keys, which a reader decodes: the reviewed cases first.
            (
                "agents:\n  root:\n    \"tool\\x73ets\":\n      - type: filesystem\n",
                3,
                QUOTED_KEY,
            ),
            (
                "agents:\n  helper:\n    model: fast\n  \"r\\x6fot\":\n    model: fast\n",
                4,
                QUOTED_KEY,
            ),
            (
                "agents:\n  helper:\n    model: fast\n  \"root\":\n    model: fast\n",
                4,
                QUOTED_KEY,
            ),
            ("agents:\n  'root':\n    model: fast\n", 2, QUOTED_KEY),
            (
                "agents:\n  root:\n    model: fast\n  helper:\n    \"model\": fast\n",
                5,
                QUOTED_KEY,
            ),
            ("\"agents\":\n  root:\n    model: fast\n", 1, QUOTED_KEY),
            (
                "toolsets:\n  web:\n    - type: fetch\nagents:\n  \"my agent\":\n    use_toolsets: [web]\n",
                5,
                QUOTED_KEY,
            ),
            // In the agent edited, values and collections over several lines.
            (
                "agents:\n  root:\n    instruction: \"first line\n      # not a comment\"\n  helper:\n    model: fast\n",
                3,
                MULTI_LINE_QUOTE,
            ),
            (
                "agents:\n  root:\n    instruction: 'it''s\n      long'\n",
                3,
                MULTI_LINE_QUOTE,
            ),
            (
                "agents:\n  root:\n    model: {provider: x,\n      name: y}\n",
                3,
                MULTI_LINE_FLOW,
            ),
        ] {
            let problem = Problem::Construct { line, form };
            let refused = Err(problem.clone());
            assert_eq!(
                with_toolset(yaml, EXE, "agentdocker").map(|_| ()),
                refused,
                "{yaml}"
            );
            assert_eq!(
                without_toolset(yaml, "agentdocker").map(|_| ()),
                refused,
                "{yaml}"
            );
            assert_eq!(target(yaml, "agentdocker").map(|_| ()), refused, "{yaml}");
            let message = problem.to_string();
            assert!(
                message.starts_with(&format!("line {line}: {form}")),
                "{message}"
            );
        }
        for yaml in [
            "agents:\n  root:\n    description: it's a test of *plain* text & more? yes\n    instruction: \"use *bold*, & 'quotes' # not a comment \\\" <<\"\n    model: 'it''s'  # a comment's *own* text\n",
            "agents:\n  root:\n    instruction: |\n      *bold* & <<more>> !tag\n      ? not a key\n      : not a value\n      &not *alias\n    model: fast\n",
            "agents:\n  root:\n    model: fast\n    url: http://localhost:8080/a?b=c\n    tags: [a, \"b, c\", {d: e}]\n",
            "agents:\n  root:\n    model: fast\n  helper:\n    instruction: \"a quoted value\n      over two lines, not in the agent edited\"\n",
        ] {
            add(yaml);
        }
    }

    /// The runtime check refuses an edit it cannot confirm, whatever the
    /// lines made of the file: more changed than our entry, the entry under
    /// an agent other than the one the file names as default, a key twice
    /// in one mapping, a text that is no YAML, a removal of more than ours,
    /// and an entry the lines and the reading disagree about.
    #[test]
    fn the_runtime_check_refuses_an_edit_it_cannot_confirm() {
        let unconfirmed = |result: Result<(), Problem>, says: &str| match result {
            Err(Problem::Unverified(why)) => assert!(why.contains(says), "{why}"),
            other => panic!("{other:?}"),
        };
        let original = "agents:\n  root:\n    model: fast\n";
        let added = with_toolset(original, EXE, "agentdocker").unwrap().unwrap();
        assert_eq!(confirm_added(original, &added, "root", EXE), Ok(()));
        unconfirmed(
            confirm_added(
                original,
                &added.replace("model: fast", "model: slow"),
                "root",
                EXE,
            ),
            "more than our toolset",
        );
        unconfirmed(
            confirm_added(original, &added, "root", "/elsewhere/agentdocker"),
            "more than our toolset",
        );
        // The file names `root` by an escape the lines do not decode, so
        // an entry under helper is under the wrong agent.
        let escaped = "agents:\n  helper:\n    model: fast\n  \"r\\x6fot\":\n    model: fast\n";
        let on_helper = format!(
            "agents:\n  helper:\n    model: fast\n{}  \"r\\x6fot\":\n    model: fast\n",
            docker_agent_toolset(EXE)
        );
        unconfirmed(
            confirm_added(escaped, &on_helper, "helper", EXE),
            "default agent is `root`, not `helper`",
        );
        // An escaped `toolsets` key and a second one added beside it.
        let doubled = format!(
            "agents:\n  root:\n    \"tool\\x73ets\":\n      - type: filesystem\n{}",
            docker_agent_toolset(EXE)
        );
        unconfirmed(
            confirm_added(
                "agents:\n  root:\n    \"tool\\x73ets\":\n      - type: filesystem\n",
                &doubled,
                "root",
                EXE,
            ),
            "duplicated key",
        );
        unconfirmed(
            confirm_added("agents: [\n", &added, "root", EXE),
            "does not read as YAML",
        );
        unconfirmed(
            confirm_added(original, "agents:\n  root:\n---\nagents:\n", "root", EXE),
            "2 YAML documents",
        );
        // A removal must take out ours and nothing else.
        let both = format!(
            "agents:\n  root:\n    toolsets:\n      - type: filesystem\n{}",
            &docker_agent_toolset(EXE)["    toolsets:\n".len()..]
        );
        let kept = without_toolset(&both, "agentdocker").unwrap().unwrap();
        assert_eq!(confirm_removed(&both, &kept, "root", "agentdocker"), Ok(()));
        unconfirmed(
            confirm_removed(&both, "agents:\n  root:\n", "root", "agentdocker"),
            "more than our toolset",
        );
        unconfirmed(
            confirm_removed(original, original, "root", "agentdocker"),
            "more than our toolset",
        );
        // What the lines say is listed, the reading must find too.
        unconfirmed(
            confirm_listed(original, "root", "agentdocker", true),
            "the YAML reading does not",
        );
        unconfirmed(
            confirm_listed(&added, "root", "agentdocker", false),
            "these lines do not",
        );
        assert_eq!(confirm_listed(&added, "root", "agentdocker", true), Ok(()));
    }

    /// The scanner reads quotes, comments and indicators only where YAML
    /// does, and names the first form the editor refuses.
    #[test]
    fn the_scanner_reads_indicators_only_where_a_node_starts() {
        for (content, comment) in [
            ("model: fast # note", 12),
            ("model: \"a # b\" # note", 15),
            ("model: it's # a 'note'", 12),
            ("model: 'it''s' # note", 15),
            ("model: \"a \\\" # b\"", 17),
            ("color: a#b", 10),
            ("# all of it", 0),
        ] {
            assert_eq!(scan(content).comment, comment, "{content}");
        }
        let read = |content: &str| {
            let found = scan(content);
            (found.form, found.open_quote, found.open_flow)
        };
        assert_eq!(read("text: a *b* & c? d"), (None, false, false));
        assert_eq!(read("- &x key: v"), (Some(ANCHOR), false, false));
        assert_eq!(read("[*x, y]"), (Some(ALIAS), false, false));
        assert_eq!(read("? key"), (Some(EXPLICIT_KEY), false, false));
        assert_eq!(read(": value"), (Some(EXPLICIT_VALUE), false, false));
        assert_eq!(read("host: ::1"), (None, false, false));
        assert_eq!(read("text: \"open"), (None, true, false));
        assert_eq!(read("text: it's"), (None, false, false));
        assert_eq!(read("map: {a: 1,"), (None, false, true));
        assert_eq!(read("text: see [docs"), (None, false, false));
        assert_eq!(read("text: !!str x"), (Some(TAG), false, false));
        assert_eq!(read("text: !!merge x"), (Some(MERGE_TAG), false, false));
        assert_eq!(read("text: a << b"), (Some(MERGE), false, false));
        assert_eq!(read("text: \"a << b\""), (None, false, false));
    }

    /// A file that ends without a newline, or with CRLF endings, is
    /// extended in its own convention; an agent with `harness:` is noted.
    #[test]
    fn line_endings_and_harness_agents_are_respected() {
        let bare = "agents:\n  root:\n    model: fast";
        let after = add(bare).unwrap();
        assert!(after.starts_with("agents:\n  root:\n    model: fast\n    toolsets:\n"));
        assert_eq!(
            remove(&after).unwrap(),
            "agents:\n  root:\n    model: fast\n",
            "the newline the insertion needed stays"
        );
        let crlf = "agents:\r\n  root:\r\n    model: fast\r\n";
        let after = add(crlf).unwrap();
        assert_eq!(
            after,
            format!(
                "{crlf}    toolsets:\r\n      - type: mcp\r\n        command: \"{EXE}\"\r\n        args: [\"mcp\", \"--runtime\", \"docker-agent\"]\r\n"
            )
        );
        assert_eq!(remove(&after).unwrap(), crlf);
        let harness =
            "agents:\n  coder:\n    harness:\n      type: claude-code\n    instruction: fix it\n";
        assert_eq!(
            target(harness, "agentdocker").unwrap(),
            Target {
                agent: "coder".into(),
                harness: true,
                lists_toolset: false,
            }
        );
        add(harness);
        // With no `root`, the first agent is the target whatever it is
        // called, and a top-level `toolsets:` group is not an agent's.
        let groups =
            "toolsets:\n  web:\n    - type: fetch\nagents:\n  my-agent:\n    use_toolsets: [web]\n";
        let after = add(groups).unwrap();
        assert!(after.starts_with("toolsets:\n  web:\n    - type: fetch\nagents:\n  my-agent:\n    use_toolsets: [web]\n    toolsets:\n"));
        assert_eq!(target(groups, "agentdocker").unwrap().agent, "my-agent");
    }
}
