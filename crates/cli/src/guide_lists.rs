//! The guide's tool and command lists, checked against the code.
//!
//! `docs/GUIDE.md` and `docs/REMOTE-CONNECTOR.md` name the MCP tools and
//! the commands by hand. These tests read those documents and compare the
//! names with what the binary offers: the tool definitions the MCP server
//! serves, the tools every agent keeps and the remote connector's set, and
//! the clap command tree. A name in the code that the guide lacks, or a
//! name in the guide the code does not have, fails the test by name.
//!
//! The parsers are tolerant on purpose: they read backticked names in the
//! sections named here and nothing else, so prose around the lists can
//! change freely. Commands are compared at the first level only — a row
//! `channel prune --before <seconds>` documents `channel`, and a later
//! alternative in the same row (`connector serve` / `status` / …) is
//! accepted when it is a subcommand of the row's command or a command of
//! its own. Nested subcommands' flags are not checked.

use std::collections::BTreeSet;

use clap::CommandFactory;

use crate::mcp::{REMOTE_TOOLS, tool_names};

const GUIDE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/GUIDE.md"));
const REMOTE_CONNECTOR: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/REMOTE-CONNECTOR.md"
));

/// The body of one `## heading` section: the lines after the heading up to
/// the next `## ` heading. Panics when the heading is missing, since a
/// renamed section would otherwise pass as an empty one.
fn section<'a>(doc: &'a str, heading: &str) -> &'a str {
    let start = doc
        .find(&format!("\n{heading}\n"))
        .unwrap_or_else(|| panic!("no `{heading}` section"))
        + 1
        + heading.len();
    let rest = &doc[start..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    &rest[..end]
}

/// Every `` `name` `` in the text whose content is one lower-case
/// identifier: a tool name, never a command line with spaces or flags.
fn identifiers(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else { break };
        let name = &after[..close];
        if !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            found.insert(name.to_owned());
        }
        rest = &after[close + 1..];
    }
    found
}

/// Blank-line delimited paragraphs of a section.
fn paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|p| !p.is_empty())
        .collect()
}

/// The one paragraph containing `marker`.
fn paragraph_with<'a>(paragraphs: &'a [String], marker: &str) -> &'a str {
    let hits: Vec<&String> = paragraphs.iter().filter(|p| p.contains(marker)).collect();
    assert_eq!(
        hits.len(),
        1,
        "expected one paragraph containing {marker:?}, found {}",
        hits.len()
    );
    hits[0]
}

/// Build the clap command tree and hand it to `f` on a thread with room
/// for it: the tree is built on the stack, and the test runner's default
/// thread overflows under it.
fn with_cli<T: Send + 'static>(f: impl FnOnce(&clap::Command) -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || f(&crate::Cli::command()))
        .expect("spawn")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|&n| n.to_owned()).collect()
}

fn assert_same(what: &str, documented: &BTreeSet<String>, in_code: &BTreeSet<String>) {
    let missing: Vec<&String> = in_code.difference(documented).collect();
    let extra: Vec<&String> = documented.difference(in_code).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{what}: the document lacks {missing:?}; the document names {extra:?} that the code does not have"
    );
}

/// The guide's `## MCP tools` section names every tool the MCP server
/// defines, and nothing else.
#[test]
fn the_guide_names_every_mcp_tool_and_no_other() {
    let documented = identifiers(section(GUIDE, "## MCP tools"));
    let in_code: BTreeSet<String> = tool_names().into_iter().collect();
    assert_same("docs/GUIDE.md `## MCP tools`", &documented, &in_code);
}

/// The guide's "which every agent keeps" paragraph names `ALWAYS_TOOLS`.
#[test]
fn the_guide_names_the_tools_every_agent_keeps() {
    let paragraphs = paragraphs(section(GUIDE, "## MCP tools"));
    let keeps = paragraph_with(&paragraphs, "which every agent keeps");
    // The paragraph opens with `roles set <role> --tools …`, which has
    // spaces and is not an identifier; the identifiers are the tools.
    let documented = identifiers(keeps);
    assert_same(
        "docs/GUIDE.md, the paragraph on the tools every agent keeps",
        &documented,
        &set(agentdocker_core::role::ALWAYS_TOOLS),
    );
}

/// The guide's remote-connector paragraph names only tools the connector
/// serves. It says "the messaging tools" for the rest, so it is a subset
/// check; the connector document carries the exact list.
#[test]
fn the_guide_connector_paragraph_names_only_remote_tools() {
    let paragraphs = paragraphs(section(GUIDE, "## MCP tools"));
    let paragraph = paragraph_with(&paragraphs, "remote connector");
    let documented = identifiers(paragraph);
    let remote = set(REMOTE_TOOLS);
    let extra: Vec<&String> = documented.difference(&remote).collect();
    assert!(
        extra.is_empty(),
        "docs/GUIDE.md names {extra:?} as remote-connector tools; REMOTE_TOOLS does not have them"
    );
}

/// The connector document's `## Tools` section lists `REMOTE_TOOLS`
/// exactly before "Everything else", and after it names only real tools the
/// connector refuses.
#[test]
fn the_connector_document_lists_the_remote_tools_exactly() {
    let paragraphs = paragraphs(section(REMOTE_CONNECTOR, "## Tools"));
    let paragraph = paragraph_with(&paragraphs, "Everything else");
    let (served, refused) = paragraph.split_once("Everything else").unwrap();
    assert_same(
        "docs/REMOTE-CONNECTOR.md `## Tools`",
        &identifiers(served),
        &set(REMOTE_TOOLS),
    );
    let all_tools: BTreeSet<String> = tool_names().into_iter().collect();
    for name in identifiers(refused) {
        assert!(
            all_tools.contains(&name),
            "docs/REMOTE-CONNECTOR.md says the connector refuses `{name}`, which is not a tool"
        );
        assert!(
            !REMOTE_TOOLS.contains(&name.as_str()),
            "docs/REMOTE-CONNECTOR.md says the connector refuses `{name}`, which REMOTE_TOOLS serves"
        );
    }
}

/// The first backticked span of a `## Command reference` table cell, as the
/// command words it documents: `` `channel prune --before <seconds>` ``
/// gives `["channel", "prune", "--before", "<seconds>"]`.
fn backticked_words(cell: &str) -> Option<Vec<&str>> {
    let open = cell.find('`')?;
    let after = &cell[open + 1..];
    let close = after.find('`')?;
    Some(after[..close].split_whitespace().collect())
}

/// The command names the guide's `## Command reference` tables document:
/// per row, the first alternative's first word, together with any later
/// alternative (`a` / `b`) whose first word is a command of its own. A
/// later alternative that is a subcommand of the row's command (`connector
/// serve` / `status`) documents that subcommand and is checked to exist;
/// one that is neither fails.
fn documented_commands(cli: &clap::Command) -> BTreeSet<String> {
    let reference = section(GUIDE, "## Command reference");
    // The environment tables follow the commands in the same section.
    let commands = reference
        .split("### Environment")
        .next()
        .expect("split yields at least one piece");
    let top: BTreeSet<&str> = cli.get_subcommands().map(clap::Command::get_name).collect();
    let mut documented = BTreeSet::new();
    for line in commands.lines() {
        let Some(row) = line.strip_prefix("| `") else {
            continue;
        };
        let Some(cell) = row.split(" | ").next() else {
            continue;
        };
        let cell = format!("`{cell}");
        let mut alternatives = cell.split(" / ");
        let first = alternatives
            .next()
            .and_then(backticked_words)
            .and_then(|words| words.first().copied())
            .unwrap_or_else(|| panic!("no command in row {line:?}"));
        documented.insert(first.to_owned());
        let row_command = cli
            .get_subcommands()
            .find(|c| c.get_name() == first || c.get_all_aliases().any(|a| a == first));
        for alternative in alternatives {
            let Some(words) = backticked_words(alternative) else {
                continue;
            };
            let Some(&word) = words.first() else { continue };
            if top.contains(word) {
                documented.insert(word.to_owned());
                continue;
            }
            let is_subcommand = row_command.is_some_and(|c| {
                c.get_subcommands()
                    .any(|s| s.get_name() == word || s.get_all_aliases().any(|a| a == word))
            });
            assert!(
                is_subcommand,
                "docs/GUIDE.md row {line:?} names `{word}`, which is neither a command nor a subcommand of `{first}`"
            );
        }
    }
    documented
}

/// Every visible clap command has a row in the guide's command reference,
/// and every row names a command that exists (hidden ones included: the
/// guide documents them and a bullet says they are hidden).
#[test]
fn the_guide_documents_every_visible_command_and_no_other() {
    with_cli(|cli| {
        let documented = documented_commands(cli);
        let existing: BTreeSet<String> = cli
            .get_subcommands()
            .flat_map(|c| {
                std::iter::once(c.get_name().to_owned())
                    .chain(c.get_all_aliases().map(str::to_owned))
            })
            .collect();
        let visible: BTreeSet<String> = cli
            .get_subcommands()
            .filter(|c| !c.is_hide_set() && c.get_name() != "help")
            .map(|c| c.get_name().to_owned())
            .collect();
        let undocumented: Vec<&String> = visible.difference(&documented).collect();
        let phantom: Vec<&String> = documented.difference(&existing).collect();
        assert!(
            undocumented.is_empty() && phantom.is_empty(),
            "docs/GUIDE.md `## Command reference`: no row for the visible commands {undocumented:?}; rows for {phantom:?}, which are not commands"
        );
    });
}

/// The hidden commands the guide's bullet names are hidden in the parser,
/// and every hidden command is named there (the `codex-*` controllers as
/// a group).
#[test]
fn the_guide_bullet_names_the_hidden_commands() {
    with_cli(|cli| {
        let bullet = GUIDE
            .lines()
            .skip_while(|l| !l.contains("lists the commands a person types"))
            .take_while(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            !bullet.is_empty(),
            "docs/GUIDE.md lacks the bullet on hidden commands"
        );
        let mut named = BTreeSet::new();
        let mut rest = bullet.as_str();
        while let Some(open) = rest.find('`') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('`') else { break };
            named.insert(after[..close].to_owned());
            rest = &after[close + 1..];
        }
        let hidden: BTreeSet<String> = cli
            .get_subcommands()
            .filter(|c| c.is_hide_set())
            .map(|c| c.get_name().to_owned())
            .collect();
        let visible_but_named: Vec<&String> = named
            .iter()
            .filter(|n| !n.starts_with("agentdocker") && !hidden.contains(*n) && *n != "codex-*")
            .collect();
        let hidden_unnamed: Vec<&String> = hidden
            .iter()
            .filter(|h| {
                !named.contains(*h) && !(h.starts_with("codex-") && named.contains("codex-*"))
            })
            .collect();
        assert!(
            visible_but_named.is_empty() && hidden_unnamed.is_empty(),
            "docs/GUIDE.md's hidden-command bullet names {visible_but_named:?}, which are not hidden; the hidden commands {hidden_unnamed:?} are not named"
        );
    });
}
