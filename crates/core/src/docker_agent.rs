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
//! The edit is done by lines, with no YAML library: everything the person
//! wrote — comments, blank lines, quoting, key order — stays byte for
//! byte, and only the lines of our entry (and a `toolsets:` key when the
//! agent had none) are added or removed. The editor reads only as much YAML
//! as it needs to place those lines with certainty, and is conservative by
//! rule. The agent it edits must be written as an indented block whose own
//! lines use no anchor (`&name`), alias (`*name`) or merge key (`<<`,
//! `!!merge`) — what such lines share with the rest of the file is not on
//! them, so an edit could change more than it adds, or leave an alias with
//! nothing to name — no explicit key (`? `), and no quoted value or inline
//! list or mapping that runs over several lines; its `toolsets` must be a
//! list of `- ` items or `[]`; its indentation must be spaces. Anything
//! else is refused with the reason, and the person adds the entry by hand
//! from the text [`docker_agent_toolset`] prints. A block scalar
//! (`instruction: |`) is read as the one value it is, by its content
//! indentation — the indentation indicator's, else its first line's — so a
//! `#` or `- ` line in it is text and the entry never lands inside it. The
//! tests hold every edit to an independent YAML parser.
//!
//! The target is the agent `docker agent run <file>` runs when no
//! `--agent` names another: the one named `root`, else the first (Docker
//! Agent's `Team::DefaultAgent`). An agent with `harness:` set runs an
//! external coding CLI instead of a model, and Docker Agent documents that
//! it ignores that agent's `toolsets`; the edit still goes in, and the
//! caller is told so it can say.

use std::fmt;

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
    /// The agent's lines hold an anchor, an alias or a merge key: what they
    /// share with the rest of the file, or take from it, is not on them, so
    /// an edit could change more than it adds or leave an alias with
    /// nothing to name.
    Shared(String),
    /// The agent's lines use YAML the editor does not read — the construct
    /// is named: an explicit key, or a quoted value or an inline list or
    /// mapping that runs over several lines.
    Unsupported(String, &'static str),
}

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
            Self::Shared(agent) => write!(
                f,
                "agent `{agent}` uses an anchor (`&`), an alias (`*`) or a merge key (`<<`), so what its lines share with the rest of the file is not on them"
            ),
            Self::Unsupported(agent, construct) => write!(
                f,
                "agent `{agent}` uses {construct}, which this editor does not rewrite"
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
/// when that agent lists it already. `marker` is the executable name an
/// entry is recognised by (`agentdocker`), whatever directory it is in.
pub fn with_toolset(yaml: &str, exe: &str, marker: &str) -> Result<Option<String>, Problem> {
    let doc = Document::parse(yaml);
    let agent = doc.default_agent()?;
    let toolsets = doc.toolsets(&agent)?;
    if toolsets.items.iter().any(|item| doc.is_ours(item, marker)) {
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
    Ok(Some(out.concat()))
}

/// The YAML with our toolset entries taken out of its default agent, or
/// `None` when it lists none. A `toolsets` left with no item goes with
/// them, as the entry's own key when `with_toolset` added it; the rest of
/// the file is untouched.
pub fn without_toolset(yaml: &str, marker: &str) -> Result<Option<String>, Problem> {
    let doc = Document::parse(yaml);
    let agent = doc.default_agent()?;
    let toolsets = doc.toolsets(&agent)?;
    let ours: Vec<&Item> = toolsets
        .items
        .iter()
        .filter(|item| doc.is_ours(item, marker))
        .collect();
    if ours.is_empty() {
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
    Ok(Some(kept))
}

/// The agent the edit targets, with what the caller would need to say
/// about it.
pub fn target(yaml: &str, marker: &str) -> Result<Target, Problem> {
    let doc = Document::parse(yaml);
    let agent = doc.default_agent()?;
    let toolsets = doc.toolsets(&agent)?;
    Ok(Target {
        harness: doc.has_key(&agent, "harness"),
        lists_toolset: toolsets.items.iter().any(|item| doc.is_ours(item, marker)),
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

    /// The agent `docker agent run` runs by default: the one named `root`
    /// among the keys under `agents:`, else the first of them.
    fn default_agent(&self) -> Result<Agent, Problem> {
        let agents = self
            .lines
            .iter()
            .position(|line| {
                !line.skip && !line.scalar && line.indent == 0 && line.block_key() == Some("agents")
            })
            .ok_or(Problem::NoAgents)?;
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
        // A name behind an anchor, an alias, a tag or an explicit key is
        // not one the editor reads, and could be `root`.
        for &index in &keys {
            let content = self.lines[index].content();
            let name = content
                .split_once(": ")
                .map_or(content.trim_end_matches(':'), |(name, _)| name);
            if name.starts_with(['&', '*', '!', '?']) || content.starts_with(':') {
                return Err(Problem::Unsupported(
                    name.to_owned(),
                    "an anchor, alias, tag or explicit key on an agent's name",
                ));
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

    /// Refuse the agent when its own lines — its key line through its
    /// last, block-scalar text aside — use YAML whose meaning reaches past
    /// them or that the editor does not read.
    fn conservative(&self, agent: &Agent) -> Result<(), Problem> {
        let unsupported = |construct| Err(Problem::Unsupported(agent.name.clone(), construct));
        for line in &self.lines[agent.key..agent.end] {
            if line.skip || line.scalar {
                continue;
            }
            let found = scan(&line.text[line.indent..]);
            if found.shares {
                return Err(Problem::Shared(agent.name.clone()));
            }
            if found.explicit {
                return unsupported("an explicit key (`? `, or a value on a `: ` line)");
            }
            if found.open_quote {
                return unsupported("a quoted value that runs over several lines");
            }
            if found.open_flow {
                return unsupported("an inline list or mapping that runs over several lines");
            }
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
        self.conservative(agent)?;
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
    /// An anchor, an alias or a merge key (`<<`, `!!merge`).
    shares: bool,
    /// An explicit key (`? `), or a `: ` where a node may start.
    explicit: bool,
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
                    found.shares = true;
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
                    found.shares |= tag == "!!merge";
                }
                '?' if start && spaced => found.explicit = true,
                ':' if spaced => {
                    found.explicit |= start;
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
                    found.shares = true;
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

#[cfg(test)]
mod tests {
    use super::*;
    use yaml_rust2::{Yaml, YamlLoader};

    const EXE: &str = "/opt/agentdocker/bin/agentdocker";

    const PLAIN: &str =
        "agents:\n  root:\n    model: openai/gpt-5-mini\n    description: A helpful assistant\n";

    // Every edit a test makes goes through `add` or `remove`, which hold it
    // to an independent YAML parser (yaml-rust2, a test-only dependency):
    // the text an edit gives must read as the original document with only
    // our entry added to, or taken from, the default agent's toolsets.

    /// The one document the parser reads in the text.
    fn load(text: &str) -> Yaml {
        let mut documents =
            YamlLoader::load_from_str(text).unwrap_or_else(|error| panic!("{error}:\n{text}"));
        assert!(documents.len() <= 1, "several documents:\n{text}");
        documents.pop().unwrap_or(Yaml::Null)
    }

    fn key(name: &str) -> Yaml {
        Yaml::String(name.to_owned())
    }

    /// The default agent's name as the parser reads the file: `root`, else
    /// the first.
    fn default_name(document: &Yaml) -> String {
        let agents = document["agents"].as_hash().expect("an agents mapping");
        if agents.contains_key(&key("root")) {
            return "root".to_owned();
        }
        let first = agents.keys().next().expect("an agent");
        first.as_str().expect("a named agent").to_owned()
    }

    /// Our entry, as the parser reads it.
    fn entry() -> Yaml {
        load(&format!(
            "type: mcp\ncommand: \"{EXE}\"\nargs: [\"mcp\", \"--runtime\", \"docker-agent\"]\n"
        ))
    }

    /// Whether a parsed item runs our server, as the editor means it.
    fn runs_us(item: &Yaml) -> bool {
        let args: Option<Vec<&str>> = item["args"]
            .as_vec()
            .and_then(|args| args.iter().map(Yaml::as_str).collect());
        let file = item["command"]
            .as_str()
            .map(|command| command.rsplit(['/', '\\']).next().unwrap_or(command));
        item["type"].as_str() == Some("mcp")
            && matches!(file, Some("agentdocker" | "agentdocker.exe"))
            && args.is_some_and(|args| args == ["mcp"] || args == ARGS)
    }

    fn lists_us(document: &Yaml) -> bool {
        document["agents"][default_name(document).as_str()]["toolsets"]
            .as_vec()
            .is_some_and(|items| items.iter().any(runs_us))
    }

    /// The default agent's mapping, made one when it was empty, for `edit`.
    fn with_agent(original: &Yaml, edit: impl FnOnce(&mut yaml_rust2::yaml::Hash)) -> Yaml {
        let mut document = original.clone();
        let name = default_name(&document);
        let agent = &mut document["agents"][name.as_str()];
        if *agent == Yaml::Null {
            *agent = Yaml::Hash(Default::default());
        }
        let Yaml::Hash(mapping) = agent else {
            panic!("the agent is not a mapping");
        };
        edit(mapping);
        if mapping.is_empty() {
            *agent = Yaml::Null;
        }
        document
    }

    /// What an addition must read as: the original with our entry added,
    /// last, to the default agent's toolsets.
    fn with_entry(original: &Yaml) -> Yaml {
        with_agent(original, |agent| match agent.get_mut(&key("toolsets")) {
            Some(Yaml::Array(items)) => items.push(entry()),
            Some(value) if *value == Yaml::Null => *value = Yaml::Array(vec![entry()]),
            Some(other) => panic!("toolsets is {other:?}"),
            None => {
                agent.insert(key("toolsets"), Yaml::Array(vec![entry()]));
            }
        })
    }

    /// What a removal must read as: the original without our entries in
    /// the default agent's toolsets, a list it leaves empty going with
    /// them and an agent it leaves empty reading as null.
    fn without_entries(original: &Yaml) -> Yaml {
        with_agent(original, |agent| {
            if let Some(Yaml::Array(items)) = agent.get_mut(&key("toolsets")) {
                items.retain(|item| !runs_us(item));
                if items.is_empty() {
                    agent.remove(&key("toolsets"));
                }
            }
        })
    }

    /// Add our entry, held to the parser: the text reads as the original
    /// with only our entry added; taking it out again reads as the original
    /// and gives its bytes back (but for the line ending a last line
    /// without one gets, and a `toolsets: []` opened into a list, whose key
    /// then goes); an entry the editor finds listed, the parser finds too.
    #[track_caller]
    fn add(yaml: &str) -> Option<String> {
        let after = with_toolset(yaml, EXE, "agentdocker")
            .unwrap_or_else(|problem| panic!("{problem}:\n{yaml}"));
        let before = load(yaml);
        let Some(edited) = &after else {
            assert!(
                lists_us(&before),
                "listed for the editor, not the parser:\n{yaml}"
            );
            return None;
        };
        assert_eq!(
            load(edited),
            with_entry(&before),
            "added:\n{yaml}\n---\n{edited}"
        );
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
        after
    }

    /// Take our entries out, held to the parser.
    #[track_caller]
    fn remove(yaml: &str) -> Option<String> {
        let after = without_toolset(yaml, "agentdocker")
            .unwrap_or_else(|problem| panic!("{problem}:\n{yaml}"));
        let before = load(yaml);
        match &after {
            Some(edited) => assert_eq!(
                load(edited),
                without_entries(&before),
                "removed:\n{yaml}\n---\n{edited}"
            ),
            None => assert!(
                !lists_us(&before),
                "listed for the parser, not the editor:\n{yaml}"
            ),
        }
        after
    }

    /// The default agent's instruction, as the parser reads it.
    fn instruction(yaml: &str) -> String {
        let document = load(yaml);
        let name = default_name(&document);
        document["agents"][name.as_str()]["instruction"]
            .as_str()
            .expect("an instruction")
            .to_owned()
    }

    /// The block setup prints is the entry at an agent's indent, quoted as
    /// YAML, with the arguments the daemon recognises.
    #[test]
    fn the_printed_toolset_is_the_entry_at_an_agents_indent() {
        let text = docker_agent_toolset("/Apps/Agent Docker/agentdocker");
        assert_eq!(
            text,
            "    toolsets:\n      - type: mcp\n        command: \"/Apps/Agent Docker/agentdocker\"\n        args: [\"mcp\", \"--runtime\", \"docker-agent\"]\n"
        );
        assert_eq!(
            load(&format!("agents:\n  root:\n{}", docker_agent_toolset(EXE)))["agents"]["root"]["toolsets"]
                [0],
            entry()
        );
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
        let quoted = "agents:\n  helper:\n    model: fast\n  \"root\":\n    model: fast\n";
        assert_eq!(target(quoted, "agentdocker").unwrap().agent, "root");
        add(quoted);
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

    /// What the agent's own lines share with the rest of the file, or spell
    /// in ways the editor does not read, is refused: an anchored item
    /// another agent aliases (taking it out would leave the alias naming
    /// nothing), an alias, any merge key, an explicit key, a quoted value
    /// or an inline collection over several lines. Text that only looks
    /// like them is text, and sharing elsewhere in the file is not the
    /// agent's.
    #[test]
    fn what_the_agents_lines_share_or_spell_explicitly_is_refused() {
        let shared = Problem::Shared("root".into());
        for yaml in [
            "agents:\n  root:\n    toolsets:\n      - &shared\n        type: filesystem\n  helper:\n    toolsets:\n      - *shared\n",
            "agents:\n  helper:\n    toolsets:\n      - &shared\n        type: filesystem\n  root:\n    toolsets:\n      - *shared\n",
            "defaults: &defaults\n  toolsets:\n    - type: filesystem\nagents:\n  root:\n    ? <<\n    : *defaults\n    model: fast\n",
            "defaults: &defaults\n  toolsets:\n    - type: filesystem\nagents:\n  root:\n    <<: *defaults\n    model: fast\n",
            "agents:\n  root:\n    model: fast\n    <<: [*a, *b]\n",
            "agents:\n  root:\n    toolsets: *shared\n",
            "agents:\n  root:\n    toolsets: &shared\n      - type: filesystem\n  helper:\n    toolsets: *shared\n",
            "agents:\n  root:\n    model: &m fast\n  helper:\n    model: *m\n",
            "agents:\n  root:\n    toolsets:\n      - type: script\n        env: {<<: {A: b}}\n",
            "agents:\n  root:\n    !!merge <<: {model: fast}\n",
        ] {
            assert_eq!(
                with_toolset(yaml, EXE, "agentdocker").unwrap_err(),
                shared,
                "{yaml}"
            );
            assert_eq!(
                without_toolset(yaml, "agentdocker").unwrap_err(),
                shared,
                "{yaml}"
            );
            assert_eq!(target(yaml, "agentdocker").unwrap_err(), shared, "{yaml}");
        }
        assert!(shared.to_string().contains("merge"));
        for (yaml, construct) in [
            (
                "agents:\n  root:\n    ? model\n    : fast\n",
                "explicit key",
            ),
            (
                "agents:\n  root:\n    model: fast\n    ? [a, b]\n    : c\n",
                "explicit key",
            ),
            (
                "agents:\n  root:\n    instruction: \"first line\n      # not a comment\"\n  helper:\n    model: fast\n",
                "quoted value",
            ),
            (
                "agents:\n  root:\n    instruction: 'it''s\n      long'\n",
                "quoted value",
            ),
            (
                "agents:\n  root:\n    model: {provider: x,\n      name: y}\n",
                "inline list or mapping",
            ),
            // A name the editor cannot read could be `root`'s, wherever it is.
            ("agents:\n  &a root:\n    model: fast\n", "agent's name"),
            (
                "agents:\n  helper:\n    model: fast\n  &r root:\n    model: fast\n",
                "agent's name",
            ),
            (
                "agents:\n  helper:\n    model: fast\n  !!str root:\n    model: fast\n",
                "agent's name",
            ),
            (
                "agents:\n  helper:\n    model: fast\n  ? root\n  : {model: fast}\n",
                "agent's name",
            ),
        ] {
            let problem = with_toolset(yaml, EXE, "agentdocker").unwrap_err();
            assert!(
                matches!(&problem, Problem::Unsupported(..)),
                "{yaml}: {problem:?}"
            );
            assert!(problem.to_string().contains(construct), "{problem}");
            assert_eq!(without_toolset(yaml, "agentdocker").unwrap_err(), problem);
        }
        for yaml in [
            "agents:\n  root:\n    description: it's a test of *plain* text & more? yes\n    instruction: \"use *bold*, & 'quotes' # not a comment \\\" <<\"\n    model: 'it''s'  # a comment's *own* text\n",
            "agents:\n  root:\n    instruction: |\n      *bold* & <<more>>\n      ? not a key\n      : not a value\n      &not *alias\n    model: fast\n",
            "defaults: &defaults\n  model: fast\nagents:\n  root:\n    model: fast\n  helper:\n    <<: *defaults\n",
            "agents:\n  root:\n    model: fast\n    url: http://localhost:8080/a?b=c\n    tags: [a, \"b, c\", {d: e}]\n",
        ] {
            add(yaml);
        }
    }

    /// The scanner reads quotes, comments and indicators only where YAML
    /// does.
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
        let flags = |content: &str| {
            let found = scan(content);
            (
                found.shares,
                found.explicit,
                found.open_quote,
                found.open_flow,
            )
        };
        assert_eq!(flags("text: a *b* & c? d"), (false, false, false, false));
        assert_eq!(flags("- &x key: v"), (true, false, false, false));
        assert_eq!(flags("[*x, y]"), (true, false, false, false));
        assert_eq!(flags("? key"), (false, true, false, false));
        assert_eq!(flags(": value"), (false, true, false, false));
        assert_eq!(flags("host: ::1"), (false, false, false, false));
        assert_eq!(flags("text: \"open"), (false, false, true, false));
        assert_eq!(flags("text: it's"), (false, false, false, false));
        assert_eq!(flags("map: {a: 1,"), (false, false, false, true));
        assert_eq!(flags("text: see [docs"), (false, false, false, false));
        assert_eq!(flags("text: !!str x"), (false, false, false, false));
        assert_eq!(flags("text: !!merge x"), (true, false, false, false));
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
        let groups = "toolsets:\n  web:\n    - type: fetch\nagents:\n  \"my agent\":\n    use_toolsets: [web]\n";
        let after = add(groups).unwrap();
        assert!(after.starts_with("toolsets:\n  web:\n    - type: fetch\nagents:\n  \"my agent\":\n    use_toolsets: [web]\n    toolsets:\n"));
        assert_eq!(target(groups, "agentdocker").unwrap().agent, "my agent");
    }
}
