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
//! agent had none) are added or removed. What the editor cannot place
//! with certainty — a `toolsets` written as a flow sequence (`[...]`), an
//! agent written as a flow mapping, an agent that takes keys from a merge
//! (`<<:`) or whose `toolsets` is an alias or carries an anchor (what the
//! agent's effective list is cannot be read off these lines), tabs for
//! indentation — is refused with the reason rather than guessed at, and
//! the person adds the entry by hand from the text
//! [`docker_agent_toolset`] prints. A block scalar (`instruction: |`) is
//! read as the one value it is, comments and all, so the entry never lands
//! inside it.
//!
//! The target is the file's first agent: what `docker agent run <file>`
//! runs when no `--agent` names another, by Docker Agent's own `run` help
//! ("defaults to the team's first agent"). An agent with `harness:` set
//! runs an external coding CLI instead of a model, and Docker Agent
//! documents that it ignores that agent's `toolsets`; the edit still goes
//! in, and the caller is told so it can say.

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
    /// The first agent is not written as a block mapping (`root:` with
    /// its keys indented beneath it).
    AgentNotBlock(String),
    /// The agent's `toolsets` is a flow sequence or a scalar the editor
    /// does not rewrite.
    ToolsetsNotBlock(String),
    /// The agent takes keys from a merge (`<<:`), or its `toolsets` is an
    /// alias or carries an anchor: what the agent's effective toolsets
    /// are is not on its own lines, so an entry could not be placed
    /// without changing more than it adds.
    Shared(String),
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
                "agent `{agent}` takes keys from a merge (`<<:`) or shares its `toolsets` through an alias or anchor, so its effective toolsets are not on its own lines"
            ),
        }
    }
}

impl std::error::Error for Problem {}

/// The file's first agent, as the edit would target it.
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

/// The YAML with our toolset entry added to its first agent, or `None`
/// when that agent lists it already. `marker` is the executable name an
/// entry is recognised by (`agentdocker`), whatever directory it is in.
pub fn with_toolset(yaml: &str, exe: &str, marker: &str) -> Result<Option<String>, Problem> {
    let doc = Document::parse(yaml);
    let agent = doc.first_agent()?;
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

/// The YAML with our toolset entries taken out of its first agent, or
/// `None` when it lists none. A `toolsets` left with no item goes with
/// them, as the entry's own key when `with_toolset` added it; the rest of
/// the file is untouched.
pub fn without_toolset(yaml: &str, marker: &str) -> Result<Option<String>, Problem> {
    let doc = Document::parse(yaml);
    let agent = doc.first_agent()?;
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

/// The first agent of the file, with what the edit would need to say
/// about it.
pub fn target(yaml: &str, marker: &str) -> Result<Target, Problem> {
    let doc = Document::parse(yaml);
    let agent = doc.first_agent()?;
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

    /// The lines of every block scalar: from the line after its `|` or
    /// `>` indicator while they stay deeper than the node it is the value
    /// of (the key, or a sequence entry's dash), blank lines between
    /// included, and the blank lines after the last when the indicator
    /// keeps them (`|+`). They are the scalar's text, so a `#` among them
    /// is not a comment and a `- ` is not an item.
    fn mark_block_scalars(&mut self) {
        let mut index = 0;
        while index < self.lines.len() {
            let line = &self.lines[index];
            let header = if line.skip || line.scalar {
                None
            } else {
                block_scalar_header(line.content())
            };
            let Some((column, keep)) = header else {
                index += 1;
                continue;
            };
            let parent = line.indent + column;
            let mut next = index + 1;
            let mut last = index;
            while next < self.lines.len() {
                let candidate = &self.lines[next];
                if candidate.text.trim().is_empty() {
                    next += 1;
                } else if candidate.indent > parent {
                    last = next;
                    next += 1;
                } else {
                    break;
                }
            }
            let end = if keep { next } else { last + 1 };
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

    fn first_agent(&self) -> Result<Agent, Problem> {
        let agents = self
            .lines
            .iter()
            .position(|line| !line.skip && line.indent == 0 && line.block_key() == Some("agents"))
            .ok_or(Problem::NoAgents)?;
        let end = self.block_end(agents + 1, 0)?;
        let key = (agents + 1..end)
            .find(|&index| !self.lines[index].skip)
            .ok_or(Problem::NoAgents)?;
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
        // Keys merged in from elsewhere (`<<: *defaults`) may carry a
        // toolsets list this file's lines do not show; a local key added
        // beside the merge would replace it, not extend it.
        if self.has_key(agent, "<<") {
            return Err(Problem::Shared(agent.name.clone()));
        }
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
        if value.starts_with('*') || value.starts_with('&') {
            return Err(Problem::Shared(agent.name.clone()));
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

/// A block scalar's header: a line's content (past its indent, comment
/// removed) that ends in a `|` or `>` indicator — with chomping and
/// indentation indicators in either order — as the value of a key
/// (`key: |`, `- key: >-`) or of a sequence entry (`- |`), an anchor or a
/// tag allowed before it. `Some((column, keep))`: the column, past the
/// line's indent, of the node the scalar belongs to — the key, or the
/// entry's dash — which the scalar's lines are deeper than; and whether
/// `+` chomping keeps the blank lines after its text.
fn block_scalar_header(content: &str) -> Option<(usize, bool)> {
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
    let digits = flags.chars().filter(char::is_ascii_digit).count();
    if flags.chars().all(|c| matches!(c, '-' | '+' | '1'..='9')) && chomping <= 1 && digits <= 1 {
        Some((parent, flags.contains('+')))
    } else {
        None
    }
}

/// The text before a ` #` comment, outside quotes.
fn strip_comment(text: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut previous = ' ';
    for (index, c) in text.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' && previous.is_whitespace() => return &text[..index],
            None => {}
        }
        previous = c;
    }
    text
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

    const EXE: &str = "/opt/agentdocker/bin/agentdocker";

    const PLAIN: &str =
        "agents:\n  root:\n    model: openai/gpt-5-mini\n    description: A helpful assistant\n";

    /// The block setup prints is the entry at an agent's indent, quoted as
    /// YAML, with the arguments the daemon recognises.
    #[test]
    fn the_printed_toolset_is_the_entry_at_an_agents_indent() {
        let text = docker_agent_toolset("/Apps/Agent Docker/agentdocker");
        assert_eq!(
            text,
            "    toolsets:\n      - type: mcp\n        command: \"/Apps/Agent Docker/agentdocker\"\n        args: [\"mcp\", \"--runtime\", \"docker-agent\"]\n"
        );
    }

    /// An agent without `toolsets` gets the key and our entry after its
    /// last line; the rest of the file is byte for byte as it was, and the
    /// edit is recognised afterwards, so a second setup adds nothing.
    #[test]
    fn an_agent_without_toolsets_gets_the_key_and_the_entry() {
        let after = with_toolset(PLAIN, EXE, "agentdocker").unwrap().unwrap();
        assert_eq!(
            after,
            format!(
                "{PLAIN}    toolsets:\n      - type: mcp\n        command: \"{EXE}\"\n        args: [\"mcp\", \"--runtime\", \"docker-agent\"]\n"
            )
        );
        assert_eq!(with_toolset(&after, EXE, "agentdocker").unwrap(), None);
        assert_eq!(
            target(&after, "agentdocker").unwrap(),
            Target {
                agent: "root".into(),
                harness: false,
                lists_toolset: true,
            }
        );
        assert_eq!(
            without_toolset(&after, "agentdocker").unwrap().unwrap(),
            PLAIN,
            "the removal is the exact reverse"
        );
        assert_eq!(without_toolset(PLAIN, "agentdocker").unwrap(), None);
    }

    /// An existing list keeps its items, comments and the person's own
    /// indent; ours goes last, and only ours goes at removal.
    #[test]
    fn an_existing_list_keeps_its_items_and_takes_ours_last() {
        let yaml = "# team\nmodels:\n  fast: openai/gpt-5-mini\n\nagents:\n  root:\n    model: fast\n    toolsets:\n      # search the web\n      - type: mcp\n        ref: docker:duckduckgo\n      - type: filesystem\n\n    sub_agents: [writer]\n\n  writer:\n    model: fast\n";
        let after = with_toolset(yaml, EXE, "agentdocker").unwrap().unwrap();
        let expected = yaml.replace(
            "      - type: filesystem\n",
            &format!(
                "      - type: filesystem\n      - type: mcp\n        command: \"{EXE}\"\n        args: [\"mcp\", \"--runtime\", \"docker-agent\"]\n"
            ),
        );
        assert_eq!(after, expected);
        assert_eq!(
            without_toolset(&after, "agentdocker").unwrap().unwrap(),
            yaml
        );
        // Items at the key's own indent are a list too.
        let flush = "agents:\n  root:\n    model: fast\n    toolsets:\n    - type: filesystem\n    instruction: go\n";
        let after = with_toolset(flush, EXE, "agentdocker").unwrap().unwrap();
        assert_eq!(
            after,
            format!(
                "agents:\n  root:\n    model: fast\n    toolsets:\n    - type: filesystem\n    - type: mcp\n      command: \"{EXE}\"\n      args: [\"mcp\", \"--runtime\", \"docker-agent\"]\n    instruction: go\n"
            )
        );
        assert_eq!(
            without_toolset(&after, "agentdocker").unwrap().unwrap(),
            flush
        );
    }

    /// `toolsets: []` opens into a list; taking the last item out takes the
    /// key with it. An inline list with items is refused, never rewritten.
    #[test]
    fn an_empty_inline_list_opens_and_a_full_one_is_refused() {
        let yaml = "agents:\n  root:\n    toolsets: []  # none yet\n    model: fast\n";
        let after = with_toolset(yaml, EXE, "agentdocker").unwrap().unwrap();
        assert_eq!(
            after,
            format!(
                "agents:\n  root:\n    toolsets: # none yet\n      - type: mcp\n        command: \"{EXE}\"\n        args: [\"mcp\", \"--runtime\", \"docker-agent\"]\n    model: fast\n"
            )
        );
        assert_eq!(
            without_toolset(&after, "agentdocker").unwrap().unwrap(),
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
    /// agent's entry is not this agent's.
    #[test]
    fn our_entry_is_known_by_what_it_runs() {
        let listed = |item: &str| {
            let yaml =
                format!("agents:\n  root:\n    toolsets:\n{item}  helper:\n    model: fast\n");
            target(&yaml, "agentdocker").unwrap().lists_toolset
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
        // Another agent's entry is not the first agent's.
        let other = format!(
            "agents:\n  root:\n    model: fast\n  helper:\n{}",
            docker_agent_toolset(EXE)
        );
        assert!(!target(&other, "agentdocker").unwrap().lists_toolset);
        assert!(with_toolset(&other, EXE, "agentdocker").unwrap().is_some());
    }

    /// A block scalar is one value whatever its lines look like: a `#`
    /// line inside an instruction is text, not a comment, so the entry
    /// goes after the whole scalar and the instruction reads the same
    /// before and after; `|+` keeps its trailing blank lines inside.
    #[test]
    fn a_block_scalar_keeps_its_text_whole() {
        // The value of `instruction` by YAML's rules for block scalars,
        // read apart from the editor: the lines deeper than the key (blank
        // ones among them), less the first one's indentation, folded for
        // `>` (the fixtures have no more-indented lines), with the
        // chomping applied — strip `-`, clip, keep `+`.
        fn instruction(yaml: &str) -> String {
            let indent = |line: &str| line.len() - line.trim_start_matches(' ').len();
            let lines: Vec<&str> = yaml.lines().collect();
            let at = lines
                .iter()
                .position(|line| line.trim_start().starts_with("instruction:"))
                .unwrap();
            let header = lines[at].trim_start()["instruction:".len()..].trim();
            let body: Vec<&str> = lines[at + 1..]
                .iter()
                .copied()
                .take_while(|line| line.trim().is_empty() || indent(line) > indent(lines[at]))
                .collect();
            let content = body
                .iter()
                .copied()
                .find(|line| !line.trim().is_empty())
                .map_or(0, indent);
            let texts: Vec<&str> = body
                .iter()
                .map(|line| {
                    if line.trim().is_empty() {
                        ""
                    } else {
                        &line[content..]
                    }
                })
                .collect();
            let last = texts
                .iter()
                .rposition(|text| !text.is_empty())
                .map_or(0, |i| i + 1);
            let mut value = String::new();
            for (i, text) in texts[..last].iter().enumerate() {
                if i > 0 {
                    let fold =
                        header.starts_with('>') && !text.is_empty() && !texts[i - 1].is_empty();
                    value.push(if fold { ' ' } else { '\n' });
                }
                value.push_str(text);
            }
            if header.contains('+') {
                value.push('\n');
                value.push_str(&"\n".repeat(texts.len() - last));
            } else if !header.contains('-') && last > 0 {
                value.push('\n');
            }
            value
        }
        let yaml = "agents:\n  root:\n    model: fast\n    instruction: |\n      Follow the rules.\n      # Keep all filenames private\n      - never a list\n  helper:\n    model: fast\n";
        let after = with_toolset(yaml, EXE, "agentdocker").unwrap().unwrap();
        assert_eq!(
            after,
            format!(
                "agents:\n  root:\n    model: fast\n    instruction: |\n      Follow the rules.\n      # Keep all filenames private\n      - never a list\n    toolsets:\n      - type: mcp\n        command: \"{EXE}\"\n        args: [\"mcp\", \"--runtime\", \"docker-agent\"]\n  helper:\n    model: fast\n"
            )
        );
        assert_eq!(
            instruction(&after),
            instruction(yaml),
            "the instruction reads the same"
        );
        assert_eq!(
            instruction(yaml),
            "Follow the rules.\n# Keep all filenames private\n- never a list\n"
        );
        assert_eq!(
            without_toolset(&after, "agentdocker").unwrap().unwrap(),
            yaml
        );
        // A scalar of `#` lines alone is still the whole value.
        let hashes = "agents:\n  root:\n    instruction: >-\n      # one\n      # two\n";
        let after = with_toolset(hashes, EXE, "agentdocker").unwrap().unwrap();
        assert!(after.starts_with(
            "agents:\n  root:\n    instruction: >-\n      # one\n      # two\n    toolsets:\n"
        ));
        assert_eq!(instruction(&after), instruction(hashes));
        assert_eq!(instruction(hashes), "# one # two");
        // `|+` keeps the blank lines after the text: the entry goes after
        // them, and blank lines inside a scalar stay inside.
        let keep = "agents:\n  root:\n    instruction: |+\n      First.\n\n      Last.\n\n\n  helper:\n    model: fast\n";
        let after = with_toolset(keep, EXE, "agentdocker").unwrap().unwrap();
        assert!(after.starts_with(
            "agents:\n  root:\n    instruction: |+\n      First.\n\n      Last.\n\n\n    toolsets:\n"
        ));
        assert_eq!(instruction(&after), instruction(keep));
        assert_eq!(instruction(keep), "First.\n\nLast.\n\n\n");
        assert_eq!(
            without_toolset(&after, "agentdocker").unwrap().unwrap(),
            keep
        );
        // A scalar inside an item is that item's, and the item's keys
        // are still read around it, on its own line or on the dash's.
        for item in [
            "agents:\n  root:\n    toolsets:\n      - type: mcp\n        instruction: |\n          # use it\n          - always\n        command: /opt/agentdocker\n        args: [mcp]\n",
            "agents:\n  root:\n    toolsets:\n      - instruction: |+\n          # use it\n\n        type: mcp\n        command: /opt/agentdocker\n        args: [mcp]\n",
        ] {
            assert!(target(item, "agentdocker").unwrap().lists_toolset, "{item}");
            assert_eq!(
                without_toolset(item, "agentdocker").unwrap().unwrap(),
                "agents:\n  root:\n",
                "{item}"
            );
        }
        for (content, header) in [
            ("instruction: |", Some((0, false))),
            ("instruction: >-", Some((0, false))),
            ("instruction: |+", Some((0, true))),
            ("instruction: |2+", Some((0, true))),
            ("instruction: &doc !!str |", Some((0, false))),
            ("\"a:b\": >-", Some((0, false))),
            ("- |", Some((0, false))),
            ("- note: >+", Some((2, true))),
            ("-   - note: |", Some((6, false))),
            ("note: a |", None),
            ("instruction: a | b", None),
            ("instruction: |x", None),
            ("instruction: |--", None),
            ("command: /bin/x", None),
            ("--flag: |x", None),
            ("|", None),
            ("-", None),
        ] {
            assert_eq!(block_scalar_header(content), header, "{content}");
        }
    }

    /// An agent whose keys come from a merge, or whose toolsets are an
    /// alias or carry an anchor, is refused: a local list would replace
    /// what it shares, not add to it.
    #[test]
    fn merged_or_shared_toolsets_are_refused() {
        let shared = Problem::Shared("root".into());
        for yaml in [
            "defaults: &defaults\n  toolsets:\n    - type: filesystem\nagents:\n  root:\n    <<: *defaults\n    model: fast\n",
            "agents:\n  root:\n    model: fast\n    <<: [*a, *b]\n",
            "agents:\n  root:\n    toolsets: *shared\n",
            "agents:\n  root:\n    toolsets: &shared\n      - type: filesystem\n  helper:\n    toolsets: *shared\n",
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
        // A merge in another agent, or an anchor elsewhere, is not this
        // agent's.
        let other = "agents:\n  root:\n    model: &m fast\n  helper:\n    <<: *defaults\n";
        assert!(with_toolset(other, EXE, "agentdocker").unwrap().is_some());
    }

    /// A file that ends without a newline, or with CRLF endings, is
    /// extended in its own convention; an agent with `harness:` is noted.
    #[test]
    fn line_endings_and_harness_agents_are_respected() {
        let bare = "agents:\n  root:\n    model: fast";
        let after = with_toolset(bare, EXE, "agentdocker").unwrap().unwrap();
        assert!(after.starts_with("agents:\n  root:\n    model: fast\n    toolsets:\n"));
        assert_eq!(
            without_toolset(&after, "agentdocker").unwrap().unwrap(),
            "agents:\n  root:\n    model: fast\n",
            "the newline the insertion needed stays"
        );
        let crlf = "agents:\r\n  root:\r\n    model: fast\r\n";
        let after = with_toolset(crlf, EXE, "agentdocker").unwrap().unwrap();
        assert_eq!(
            after,
            format!(
                "{crlf}    toolsets:\r\n      - type: mcp\r\n        command: \"{EXE}\"\r\n        args: [\"mcp\", \"--runtime\", \"docker-agent\"]\r\n"
            )
        );
        assert_eq!(
            without_toolset(&after, "agentdocker").unwrap().unwrap(),
            crlf
        );
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
        // The first agent is the target whatever it is called, and a
        // top-level `toolsets:` group is not an agent's.
        let groups = "toolsets:\n  web:\n    - type: fetch\nagents:\n  \"my agent\":\n    use_toolsets: [web]\n";
        let after = with_toolset(groups, EXE, "agentdocker").unwrap().unwrap();
        assert!(after.starts_with("toolsets:\n  web:\n    - type: fetch\nagents:\n  \"my agent\":\n    use_toolsets: [web]\n    toolsets:\n"));
        assert_eq!(target(groups, "agentdocker").unwrap().agent, "my agent");
    }
}
