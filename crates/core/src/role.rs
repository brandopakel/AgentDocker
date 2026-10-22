//! A project's roles: what an agent can be given to do, each with a colour.
//! Every project offers five to pick from; the person can rename, recolour,
//! describe or remove them and add their own. A role is the person's to
//! give: nothing holds one until they say so. A role can also name the
//! MCP tools its holders are offered; one that names none offers them all.
use crate::ProjectId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// At most this many roles in a project, removed defaults included.
pub const ROLES_PER_PROJECT: usize = 24;
/// A role's description is one line of at most this many characters.
pub const DESCRIPTION_CHARS: usize = 120;
/// A role's name, as people read it, in characters.
pub const NAME_CHARS: usize = 32;
/// At most this many MCP tools a role names.
pub const TOOLS_PER_ROLE: usize = 128;
/// A tool's name, in characters.
pub const TOOL_CHARS: usize = 64;

/// The tools an agent keeps whatever its roles limit it to: enough to know
/// who it is, find the others and its roles, and talk and be talked to.
pub const ALWAYS_TOOLS: &[&str] = &[
    "whoami",
    "send_message",
    "read_inbox",
    "wait_for_messages",
    "acknowledge_messages",
    "list_agents",
    "list_roles",
];

/// A role's colour, by name: Claude Code's eight, so an agent's colour in
/// its own terminal can match the app's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleColor {
    Red,
    Blue,
    Green,
    Yellow,
    Purple,
    Orange,
    Pink,
    Cyan,
}

impl RoleColor {
    pub const ALL: [RoleColor; 8] = [
        RoleColor::Red,
        RoleColor::Blue,
        RoleColor::Green,
        RoleColor::Yellow,
        RoleColor::Purple,
        RoleColor::Orange,
        RoleColor::Pink,
        RoleColor::Cyan,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RoleColor::Red => "red",
            RoleColor::Blue => "blue",
            RoleColor::Green => "green",
            RoleColor::Yellow => "yellow",
            RoleColor::Purple => "purple",
            RoleColor::Orange => "orange",
            RoleColor::Pink => "pink",
            RoleColor::Cyan => "cyan",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().to_lowercase();
        Self::ALL.into_iter().find(|c| c.as_str() == text)
    }

    /// The next colour round the palette, for the role after `count`.
    pub fn after(count: usize) -> Self {
        Self::ALL[count % Self::ALL.len()]
    }
}

/// One role of a project. `role` is its address (`role:reviewer`) and never
/// changes; the name, colour and description are the person's to change. A
/// removed built-in role is kept as `removed`, so it stays gone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleDef {
    pub project: ProjectId,
    pub role: String,
    pub name: String,
    pub color: RoleColor,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
    /// The MCP tools its holders are offered, besides [`ALWAYS_TOOLS`];
    /// absent, every tool. Never empty: clearing the list lifts the limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// Who changed it last; empty for a built-in role nobody has changed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The store's id for a project's role.
pub fn role_id(project: &ProjectId, role: &str) -> String {
    format!("{}:{role}", project.as_str())
}

/// The five roles every project offers, unchanged.
pub fn defaults(project: &ProjectId, at: DateTime<Utc>) -> Vec<RoleDef> {
    [
        (
            "lead",
            "Lead",
            RoleColor::Purple,
            "Plans the work and hands it out",
        ),
        (
            "implementer",
            "Implementer",
            RoleColor::Blue,
            "Writes the changes",
        ),
        (
            "reviewer",
            "Reviewer",
            RoleColor::Orange,
            "Reviews changes before they merge",
        ),
        (
            "explorer",
            "Explorer",
            RoleColor::Cyan,
            "Researches read-only; changes nothing",
        ),
        ("tester", "Tester", RoleColor::Green, "Tests and reports"),
    ]
    .into_iter()
    .map(|(role, name, color, description)| RoleDef {
        project: project.clone(),
        role: role.to_owned(),
        name: name.to_owned(),
        color,
        description: description.to_owned(),
        removed: false,
        tools: None,
        created_by: String::new(),
        created_at: at,
        updated_at: at,
    })
    .collect()
}

/// The project's roles: the built-in ones as the person left them, then
/// the person's own, in the order they were made. A stored role replaces
/// the built-in one of the same address; a removed one is left out.
pub fn catalog(project: &ProjectId, stored: Vec<RoleDef>, at: DateTime<Utc>) -> Vec<RoleDef> {
    let mut roles = defaults(project, at);
    let mut own: Vec<RoleDef> = Vec::new();
    for role in stored.into_iter().filter(|r| &r.project == project) {
        match roles.iter_mut().find(|r| r.role == role.role) {
            Some(slot) => *slot = role,
            None => own.push(role),
        }
    }
    own.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.role.cmp(&b.role)));
    roles.extend(own);
    roles.retain(|r| !r.removed);
    roles
}

/// How many addresses a project's roles take: the five built in (changed,
/// taken out or not) and the person's own. Its stored documents are never
/// more than this, so it is the bound to make a role against.
pub fn addresses(project: &ProjectId, stored: &[RoleDef]) -> usize {
    let mut addresses: std::collections::BTreeSet<&str> =
        ["lead", "implementer", "reviewer", "explorer", "tester"]
            .into_iter()
            .collect();
    addresses.extend(
        stored
            .iter()
            .filter(|r| &r.project == project)
            .map(|r| r.role.as_str()),
    );
    addresses.len()
}

/// The address a name makes: lowercase letters, digits and single hyphens,
/// at most forty characters, as [`crate::agent::check_role`] accepts.
pub fn slug_from(name: &str) -> Option<String> {
    let mut slug = String::new();
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug
        .trim_end_matches('-')
        .chars()
        .take(crate::agent::ROLE_CHARS)
        .collect();
    let slug = slug.trim_end_matches('-').to_owned();
    crate::agent::check_role(&slug).ok().map(|()| slug)
}

/// Whether a name and description can be shown: a name of one to
/// [`NAME_CHARS`] characters, a description on one line of at most
/// [`DESCRIPTION_CHARS`], neither with control characters.
pub fn check_words(name: &str, description: &str) -> Result<(), &'static str> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > NAME_CHARS {
        return Err("a role's name is one to thirty-two characters");
    }
    if description.chars().count() > DESCRIPTION_CHARS {
        return Err("a role's description is at most 120 characters");
    }
    if name
        .chars()
        .chain(description.chars())
        .any(char::is_control)
    {
        return Err("a role's name and description are one line of text");
    }
    Ok(())
}

/// A role's tool list as it is kept, sorted with repeats dropped: at most
/// [`TOOLS_PER_ROLE`] names, each one to [`TOOL_CHARS`] lowercase letters,
/// digits and underscores, the shape of every MCP tool's name. Which tools
/// exist is the MCP server's to know, not this.
pub fn check_tools(tools: &[String]) -> Result<Vec<String>, &'static str> {
    if tools.len() > TOOLS_PER_ROLE {
        return Err("a role names at most 128 tools");
    }
    let mut kept = std::collections::BTreeSet::new();
    for tool in tools {
        let tool = tool.trim();
        if tool.is_empty()
            || tool.len() > TOOL_CHARS
            || !tool
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(
                "a tool's name is one to sixty-four lowercase letters, digits and underscores",
            );
        }
        kept.insert(tool.to_owned());
    }
    Ok(kept.into_iter().collect())
}

/// The tools an agent holding `held` is offered from a project's roles:
/// `None` for every tool, when it holds no role or any of its roles names
/// none (a role the palette lacks names none); otherwise every tool its
/// roles name, together, with [`ALWAYS_TOOLS`].
pub fn tool_limit(
    held: &[&str],
    palette: &[RoleDef],
) -> Option<std::collections::BTreeSet<String>> {
    if held.is_empty() {
        return None;
    }
    let mut allowed: std::collections::BTreeSet<String> =
        ALWAYS_TOOLS.iter().map(|&tool| tool.to_owned()).collect();
    for role in held {
        let tools = palette
            .iter()
            .find(|def| def.role == *role)
            .and_then(|def| def.tools.as_ref())?;
        allowed.extend(tools.iter().cloned());
    }
    Some(allowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_palette_is_claude_codes_eight_and_parses_by_name() {
        assert_eq!(
            RoleColor::ALL.map(RoleColor::as_str),
            [
                "red", "blue", "green", "yellow", "purple", "orange", "pink", "cyan"
            ]
        );
        assert_eq!(RoleColor::parse(" Cyan "), Some(RoleColor::Cyan));
        assert_eq!(RoleColor::parse("teal"), None);
        assert_eq!(RoleColor::after(9), RoleColor::Blue);
    }

    #[test]
    fn the_catalog_lays_the_persons_changes_over_the_five() {
        let project = ProjectId::from("p");
        let now = Utc::now();
        let roles = catalog(&project, Vec::new(), now);
        assert_eq!(
            roles.iter().map(|r| r.role.as_str()).collect::<Vec<_>>(),
            ["lead", "implementer", "reviewer", "explorer", "tester"]
        );
        let mut reviewer = roles[2].clone();
        reviewer.name = "Code review".into();
        reviewer.color = RoleColor::Pink;
        let mut tester = roles[4].clone();
        tester.removed = true;
        let own = RoleDef {
            role: "docs".into(),
            name: "Docs".into(),
            color: RoleColor::Yellow,
            description: String::new(),
            created_by: "user".into(),
            ..roles[0].clone()
        };
        let mut elsewhere = own.clone();
        elsewhere.project = ProjectId::from("other");
        elsewhere.role = "elsewhere".into();
        let roles = catalog(&project, vec![reviewer, tester, own, elsewhere], now);
        assert_eq!(
            roles.iter().map(|r| r.role.as_str()).collect::<Vec<_>>(),
            ["lead", "implementer", "reviewer", "explorer", "docs"]
        );
        assert_eq!(roles[2].name, "Code review");
        assert_eq!(roles[2].color, RoleColor::Pink);
    }

    #[test]
    fn the_built_in_five_count_against_the_bound() {
        let project = ProjectId::from("p");
        let now = Utc::now();
        assert_eq!(addresses(&project, &[]), 5);
        let mut stored = defaults(&project, now);
        stored[0].removed = true;
        stored.push(RoleDef {
            role: "docs".into(),
            ..stored[1].clone()
        });
        assert_eq!(
            addresses(&project, &stored),
            6,
            "a removal is still an address"
        );
        assert_eq!(
            defaults(&project, now)
                .iter()
                .map(|r| r.role.as_str())
                .collect::<Vec<_>>(),
            ["lead", "implementer", "reviewer", "explorer", "tester"],
            "addresses() counts these five"
        );
    }

    #[test]
    fn a_name_makes_an_address() {
        assert_eq!(
            slug_from("Security reviewer").as_deref(),
            Some("security-reviewer")
        );
        assert_eq!(slug_from("  QA / Tests!  ").as_deref(), Some("qa-tests"));
        assert_eq!(slug_from("—"), None);
        assert!(check_words("Reviewer", "Reviews changes").is_ok());
        assert!(check_words("", "x").is_err());
        assert!(check_words("Two\nlines", "").is_err());
        assert!(check_words("R", &"x".repeat(121)).is_err());
    }

    #[test]
    fn a_roles_tools_are_names_kept_sorted_once() {
        let names = |tools: &[&str]| tools.iter().map(|&t| t.to_owned()).collect::<Vec<_>>();
        assert_eq!(
            check_tools(&names(&["review_gate", " claim", "claim", "list_tasks2"])).unwrap(),
            ["claim", "list_tasks2", "review_gate"]
        );
        assert_eq!(check_tools(&[]).unwrap(), Vec::<String>::new());
        for bad in ["", "Claim", "send-message", "a b", "é", &"x".repeat(65)] {
            assert!(check_tools(&names(&[bad])).is_err(), "{bad:?}");
        }
        assert!(check_tools(&names(&["x"; 129])).is_err());
        assert!(check_tools(&names(&[&"x".repeat(64)])).is_ok());
    }

    #[test]
    fn an_agent_is_offered_its_roles_tools_together_or_every_tool() {
        let project = ProjectId::from("p");
        let mut palette = defaults(&project, Utc::now());
        palette[2].tools = Some(vec!["review_gate".into(), "get_task".into()]);
        palette[4].tools = Some(vec!["validate".into()]);
        let always = || ALWAYS_TOOLS.iter().map(|&t| t.to_owned());

        assert_eq!(tool_limit(&[], &palette), None, "no role, every tool");
        let reviewer = tool_limit(&["reviewer"], &palette).unwrap();
        assert!(always().all(|t| reviewer.contains(&t)));
        assert!(reviewer.contains("review_gate") && !reviewer.contains("validate"));
        assert_eq!(reviewer.len(), ALWAYS_TOOLS.len() + 2);

        let both = tool_limit(&["reviewer", "tester"], &palette).unwrap();
        assert!(both.contains("review_gate") && both.contains("validate"));
        assert_eq!(both.len(), ALWAYS_TOOLS.len() + 3);

        assert_eq!(
            tool_limit(&["reviewer", "implementer"], &palette),
            None,
            "a role without a limit offers every tool"
        );
        assert_eq!(
            tool_limit(&["reviewer", "gone"], &palette),
            None,
            "a role the palette lacks limits nothing"
        );
    }
}
