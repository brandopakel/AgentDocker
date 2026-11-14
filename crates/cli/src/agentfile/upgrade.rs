//! `agentdocker agentfile upgrade`: a version 1 file rewritten as version 2,
//! meaning what it meant. Version 1 never expanded a `$`, so every `$` in a
//! value version 2 expands ([`super::EXPANDED_FIELDS`]) is doubled, and
//! `version = 2` goes at the top. The file is edited in place through
//! `toml_edit`, which keeps comments, blank lines, key order and quoting:
//! everything that is not the two changes above reads back as it was.

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Item, Value};

use super::{EXPANDED_FIELDS, LATEST, SCHEMA_URL, declared_version};

/// What an upgrade did to a file.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Upgraded {
    /// The version the file declared before.
    pub from: u32,
    /// How many `$` were doubled.
    pub escaped: usize,
    /// The file as the latest version.
    pub text: String,
}

/// The file as the latest version, or `None` when it already is one. The
/// text must already have read as an Agentfile: this only rewrites.
pub fn upgrade(text: &str) -> Result<Option<Upgraded>> {
    let raw: toml::Table = toml::from_str(text).context("not TOML")?;
    let from = declared_version(&raw)?;
    if from == LATEST {
        return Ok(None);
    }
    let mut doc: DocumentMut = text.parse().context("not TOML")?;
    let mut escaped = 0;
    if let Some(agents) = doc
        .as_table_mut()
        .get_mut("agents")
        .and_then(Item::as_table_like_mut)
    {
        for (_, entry) in agents.iter_mut() {
            let Some(entry) = entry.as_table_like_mut() else {
                continue;
            };
            for field in EXPANDED_FIELDS {
                if let Some(item) = entry.get_mut(field) {
                    escaped += escape_item(item);
                }
            }
        }
    }
    declare_version(&mut doc);
    Ok(Some(Upgraded {
        from,
        escaped,
        text: doc.to_string(),
    }))
}

/// Double every `$` in the strings of one value, however deep, keeping
/// each string's quoting and the comments around it. Returns how many.
fn escape_item(item: &mut Item) -> usize {
    match item {
        Item::Value(value) => escape_value(value),
        Item::Table(table) => table.iter_mut().map(|(_, item)| escape_item(item)).sum(),
        Item::ArrayOfTables(tables) => tables
            .iter_mut()
            .flat_map(|table| table.iter_mut())
            .map(|(_, item)| escape_item(item))
            .sum(),
        Item::None => 0,
    }
}

fn escape_value(value: &mut Value) -> usize {
    match value {
        Value::String(text) => {
            let dollars = text.value().matches('$').count();
            if dollars > 0 {
                let decor = text.decor().clone();
                let mut replaced = toml_edit::Formatted::new(text.value().replace('$', "$$"));
                *replaced.decor_mut() = decor;
                *text = replaced;
            }
            dollars
        }
        Value::Array(items) => items.iter_mut().map(escape_value).sum(),
        Value::InlineTable(table) => table.iter_mut().map(|(_, value)| escape_value(value)).sum(),
        _ => 0,
    }
}

/// `version = <LATEST>` as the first line of the file, after the comment
/// that led it, with the `#:schema` line before both.
fn declare_version(doc: &mut DocumentMut) {
    let root = doc.as_table_mut();
    let mut prefix = String::new();
    if let Some(Item::Value(version)) = root.get_mut("version") {
        // Declared 1 outright: the value changes, its place does not.
        let decor = version.decor().clone();
        let mut replaced = toml_edit::Formatted::new(i64::from(LATEST));
        *replaced.decor_mut() = decor;
        *version = Value::Integer(replaced);
    } else {
        // The comment that led the file belongs to whatever came first;
        // `version` takes its place, and the comment with it.
        if let Some(leading) = take_leading_comment(root) {
            prefix = leading;
        }
        root.insert("version", toml_edit::value(i64::from(LATEST)));
        root.sort_values_by(|a, _, b, _| (a.get() != "version").cmp(&(b.get() != "version")));
    }
    if let Some((mut key, _)) = root.get_key_value_mut("version") {
        let own = key
            .leaf_decor()
            .prefix()
            .and_then(|raw| raw.as_str())
            .unwrap_or("");
        let has_schema = own.contains("#:schema") || prefix.contains("#:schema");
        let mut lead = String::new();
        if !has_schema {
            lead.push_str(&format!("#:schema {SCHEMA_URL}\n"));
        }
        lead.push_str(&prefix);
        lead.push_str(own);
        key.leaf_decor_mut().set_prefix(lead);
    }
}

/// The comment and blank lines before the first thing in the document, taken
/// off it: on a key/value, its key's prefix; on a `[table]` header, the
/// table's, through implicit tables (`[agents.a]` makes `agents` without a
/// header of its own) to the one that is written.
fn take_leading_comment(root: &mut toml_edit::Table) -> Option<String> {
    let first = root.iter().next().map(|(key, _)| key.to_owned())?;
    let (mut key, item) = root.get_key_value_mut(&first)?;
    match item {
        Item::Value(_) => {
            let prefix = key
                .leaf_decor()
                .prefix()
                .and_then(|raw| raw.as_str())
                .unwrap_or("")
                .to_owned();
            key.leaf_decor_mut().set_prefix("");
            Some(prefix)
        }
        Item::Table(table) => take_table_comment(table),
        Item::ArrayOfTables(tables) => tables.iter_mut().next().and_then(take_table_comment),
        Item::None => None,
    }
}

fn take_table_comment(table: &mut toml_edit::Table) -> Option<String> {
    if table.is_implicit() {
        let first = table.iter().next().map(|(key, _)| key.to_owned())?;
        return match table.get_mut(&first)? {
            Item::Table(inner) => take_table_comment(inner),
            _ => None,
        };
    }
    let prefix = table
        .decor()
        .prefix()
        .and_then(|raw| raw.as_str())
        .unwrap_or("")
        .to_owned();
    table.decor_mut().set_prefix("");
    Some(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentfile::Agentfile;

    #[test]
    fn a_version_one_file_gets_its_version_and_its_dollars_doubled_with_comments_kept() {
        let text = r#"# The team that builds the thing.
name = "backend"   # every agent gets team=backend

# The writer.
[agents.writer]
runtime = "claude-code"
command = ['claude', "-p", "spend $5 on ${TASK}"]   # literal, in version 1
env = { PRICE = "$5", PLAIN = "x" }

[agents.reviewer.labels]
cost = "$$"

[agents.reviewer]
command = ["sh", "-c", "echo $HOME"]
after = "writer exits 0"
"#;
        let upgraded = upgrade(text).unwrap().expect("version 1 upgrades");
        assert_eq!(upgraded.from, 1);
        assert_eq!(upgraded.escaped, 1 + 1 + 1 + 2 + 1);
        let expected = format!(
            r#"#:schema {SCHEMA_URL}
# The team that builds the thing.
version = 2
name = "backend"   # every agent gets team=backend

# The writer.
[agents.writer]
runtime = "claude-code"
command = ['claude', "-p", "spend $$5 on $${{TASK}}"]   # literal, in version 1
env = {{ PRICE = "$$5", PLAIN = "x" }}

[agents.reviewer.labels]
cost = "$$$$"

[agents.reviewer]
command = ["sh", "-c", "echo $$HOME"]
after = "writer exits 0"
"#
        );
        assert_eq!(upgraded.text, expected);
        // It reads as version 2 and means what version 1 meant.
        let before = Agentfile::parse(text).unwrap();
        let after = Agentfile::parse(&upgraded.text).unwrap();
        assert_eq!(after.declared, 2);
        for (name, entry) in &before.agents {
            assert_eq!(entry.command, after.agents[name].command, "{name}");
            assert_eq!(entry.env, after.agents[name].env, "{name}");
            assert_eq!(entry.labels, after.agents[name].labels, "{name}");
        }
        // Upgrading again changes nothing.
        assert!(upgrade(&upgraded.text).unwrap().is_none());
    }

    #[test]
    fn a_file_that_starts_with_a_table_or_declares_one_is_handled() {
        let headed = "# Just agents.\n[agents.a]\ncommand = [\"x\"]\n";
        let upgraded = upgrade(headed).unwrap().unwrap();
        assert_eq!(
            upgraded.text,
            format!(
                "#:schema {SCHEMA_URL}\n# Just agents.\nversion = 2\n[agents.a]\ncommand = [\"x\"]\n"
            )
        );
        let declared = "version = 1 # pinned\nname = \"t\"\n";
        let upgraded = upgrade(declared).unwrap().unwrap();
        assert_eq!(
            upgraded.text,
            format!("#:schema {SCHEMA_URL}\nversion = 2 # pinned\nname = \"t\"\n")
        );
        let empty = upgrade("").unwrap().unwrap();
        assert_eq!(empty.text, format!("#:schema {SCHEMA_URL}\nversion = 2\n"));
        let with_schema = format!("#:schema {SCHEMA_URL}\nname = \"t\"\n");
        let upgraded = upgrade(&with_schema).unwrap().unwrap();
        assert_eq!(
            upgraded.text.matches("#:schema").count(),
            1,
            "{}",
            upgraded.text
        );
        assert!(upgrade("version = 7\n").is_err());
    }
}
