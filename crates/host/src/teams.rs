//! Claude Code's team files, read where they are and never written.
//!
//! The model and the parsers are in [`agentdocker_core::team`]; this is
//! the I/O: which directory the teams live in, listing it, reading each
//! team's file and inboxes, and a project's own `.claude/` registry and
//! mailbox. Every read is bounded and read-only — a missing directory is
//! no teams, a file too large or not JSON is skipped — and nothing here
//! creates a file or a directory under `.claude/`.

use std::path::{Path, PathBuf};

use agentdocker_core::team::{
    self, INBOXES_DIR, MAILBOX_DIR, REGISTRY_FILE, TEAM_FILE, TEAMS_DIR, Team, Teammate,
};

/// A team file is small; one larger than this is not read.
const MAX_FILE: u64 = 1024 * 1024;
/// More teams than anyone runs at once; a directory with more is a
/// mistake, and the rest are left unread rather than scanned.
const MAX_TEAMS: usize = 64;
/// Inboxes read per team, and entries read from a project mailbox.
const MAX_INBOXES: usize = 256;

/// Claude Code's configuration directory: `$CLAUDE_CONFIG_DIR`, else
/// `~/.claude`; `None` where neither can be told.
pub fn claude_config_dir() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|p| !p.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => std::env::home_dir().map(|home| home.join(".claude")),
    }
}

/// Every team under `config_dir/teams/`, each with the inboxes its
/// directory has, in directory order. A directory with no readable team
/// file is skipped.
pub fn read_teams(config_dir: &Path) -> Vec<Team> {
    let Ok(entries) = std::fs::read_dir(config_dir.join(TEAMS_DIR)) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .take(MAX_TEAMS)
        .collect();
    dirs.sort();
    dirs.iter().filter_map(|dir| read_team(dir)).collect()
}

/// One team from its directory: its file, and the inboxes beside it.
pub fn read_team(dir: &Path) -> Option<Team> {
    let name = dir.file_name()?.to_str()?;
    let json = read_small(&dir.join(TEAM_FILE))?;
    let mut team = Team::from_team_file(name, &json).ok()?;
    // As a record's working directory is: `/tmp` and `/private/tmp` are
    // one checkout, and the comparison that places a lead is exact.
    let canonical = |member: &mut Teammate| {
        if let Some(cwd) = member.cwd.take() {
            member.cwd = Some(crate::project::canonical(&cwd));
        }
    };
    canonical(&mut team.lead);
    team.members.iter_mut().for_each(canonical);
    Some(team.with_inboxes(&json_files(&dir.join(INBOXES_DIR))))
}

/// What a checkout's own `.claude/` says: the registry's entries, and
/// the agents its mailbox has a file for, as members known by name
/// alone. Both are read tolerantly (see the core module); a checkout
/// without them says nothing.
pub fn read_project_agents(checkout: &Path) -> Vec<(Option<String>, Teammate)> {
    let claude = checkout.join(".claude");
    let mut found: Vec<(Option<String>, Teammate)> = read_small(&claude.join(REGISTRY_FILE))
        .map(|json| team::registry_entries(&json))
        .unwrap_or_default();
    for path in json_files(&claude.join(MAILBOX_DIR)) {
        let Some(stem) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".json"))
            .map(str::trim)
            .filter(|n| !n.is_empty())
        else {
            continue;
        };
        match found
            .iter_mut()
            .find(|(_, m)| team::same_name(&m.name, stem))
        {
            Some((_, member)) => member.inbox_path = Some(path),
            None => found.push((
                None,
                Teammate {
                    name: stem.to_owned(),
                    inbox_path: Some(path),
                    ..Teammate::default()
                },
            )),
        }
    }
    found
}

/// The teams a checkout's sessions may belong to: every team under the
/// configuration directory, with what the checkout's own `.claude/`
/// adds — a member named there and not in a team file joins the team the
/// entry names, or, naming none, the one team whose lead works in that
/// checkout. Nothing is invented: an entry that fits no team is dropped.
pub fn teams_for_checkout(config_dir: &Path, checkout: &Path) -> Vec<Team> {
    let mut teams = read_teams(config_dir);
    for (named_team, member) in read_project_agents(checkout) {
        let index = match named_team {
            Some(name) => teams.iter().position(|t| team::same_name(&t.name, &name)),
            None => {
                let mut here = teams
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.lead.cwd.as_deref() == Some(checkout));
                match (here.next(), here.next()) {
                    (Some((i, _)), None) => Some(i),
                    _ => None,
                }
            }
        };
        let Some(index) = index else {
            continue;
        };
        let team = &mut teams[index];
        if team.member(&member.name).is_none() {
            team.members.push(member);
        }
    }
    teams
}

/// The `.json` files directly in `dir`, sorted; none for no directory.
fn json_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json") && p.is_file())
        .take(MAX_INBOXES)
        .collect();
    files.sort();
    files
}

/// A regular file of at most [`MAX_FILE`] bytes, as text.
fn read_small(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team_file(lead_session: &str, cwd: &Path, members: &str) -> String {
        format!(
            r#"{{"name":"orchard","createdAt":1,"leadAgentId":"team-lead@orchard","leadSessionId":"{lead_session}",
                "members":[{{"agentId":"team-lead@orchard","name":"team-lead","agentType":"team-lead","joinedAt":1,
                "tmuxPaneId":"leader","cwd":{cwd},"subscriptions":[],"backendType":"in-process"}}{members}]}}"#,
            cwd = serde_json::to_string(&cwd.to_string_lossy()).unwrap()
        )
    }

    #[test]
    fn teams_are_read_from_their_files_and_inboxes_without_writing_anything() {
        let config = tempfile::tempdir().unwrap();
        let checkout = tempfile::tempdir().unwrap();
        let orchard = config.path().join(TEAMS_DIR).join("orchard");
        std::fs::create_dir_all(orchard.join(INBOXES_DIR)).unwrap();
        std::fs::write(
            orchard.join(TEAM_FILE),
            team_file(
                "lead-session",
                checkout.path(),
                r#",{"agentId":"pruner@orchard","name":"pruner","color":"blue","joinedAt":2}"#,
            ),
        )
        .unwrap();
        std::fs::write(orchard.join(INBOXES_DIR).join("team-lead.json"), "[]").unwrap();
        std::fs::write(orchard.join(INBOXES_DIR).join("pruner.json"), "[]").unwrap();
        std::fs::write(orchard.join(INBOXES_DIR).join("scout.json"), "[]").unwrap();
        // A directory with no team file, a stray file, and a team file
        // that is not JSON are all skipped.
        std::fs::create_dir_all(config.path().join(TEAMS_DIR).join("empty")).unwrap();
        std::fs::write(config.path().join(TEAMS_DIR).join("stray.json"), "{}").unwrap();
        let broken = config.path().join(TEAMS_DIR).join("broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join(TEAM_FILE), "{nope").unwrap();

        let teams = read_teams(config.path());
        assert_eq!(teams.len(), 1);
        let team = &teams[0];
        assert_eq!(team.name, "orchard");
        assert_eq!(team.lead_session_id.as_deref(), Some("lead-session"));
        assert_eq!(
            team.lead.cwd.as_deref(),
            Some(crate::project::canonical(checkout.path()).as_path())
        );
        assert_eq!(
            team.lead.inbox_path.as_deref(),
            Some(orchard.join(INBOXES_DIR).join("team-lead.json").as_path())
        );
        assert_eq!(
            team.member("pruner").unwrap().colour.as_deref(),
            Some("blue")
        );
        assert!(team.member("pruner").unwrap().inbox_path.is_some());
        assert!(team.member("scout").is_some(), "an inbox alone names one");

        // Nothing was made: the directories hold exactly what the test wrote.
        let count = |dir: &Path| std::fs::read_dir(dir).unwrap().count();
        assert_eq!(count(&orchard), 2);
        assert_eq!(count(&orchard.join(INBOXES_DIR)), 3);
        assert!(!checkout.path().join(".claude").exists());
        assert!(read_teams(&config.path().join("missing")).is_empty());
    }

    #[test]
    fn a_checkouts_own_registry_and_mailbox_join_the_team_led_there() {
        let config = tempfile::tempdir().unwrap();
        let checkout = tempfile::tempdir().unwrap();
        let orchard = config.path().join(TEAMS_DIR).join("orchard");
        std::fs::create_dir_all(&orchard).unwrap();
        std::fs::write(
            orchard.join(TEAM_FILE),
            team_file("lead-session", checkout.path(), ""),
        )
        .unwrap();
        let claude = checkout.path().join(".claude");
        std::fs::create_dir_all(claude.join(MAILBOX_DIR)).unwrap();
        std::fs::write(
            claude.join(REGISTRY_FILE),
            r#"{"agents":[{"name":"grafter","color":"green"},{"name":"stranger","teamName":"elsewhere"}]}"#,
        )
        .unwrap();
        std::fs::write(claude.join(MAILBOX_DIR).join("grafter.json"), "[]").unwrap();
        std::fs::write(claude.join(MAILBOX_DIR).join("scout.json"), "[]").unwrap();
        std::fs::write(claude.join(MAILBOX_DIR).join("notes.md"), "").unwrap();

        let agents = read_project_agents(checkout.path());
        assert_eq!(
            agents
                .iter()
                .map(|(_, m)| m.name.as_str())
                .collect::<Vec<_>>(),
            ["grafter", "stranger", "scout"]
        );
        assert!(
            agents[0].1.inbox_path.is_some(),
            "the mailbox file attaches by name"
        );

        let teams = teams_for_checkout(config.path(), &crate::project::canonical(checkout.path()));
        assert_eq!(teams.len(), 1);
        let names: Vec<&str> = teams[0].members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            ["grafter", "scout"],
            "the stranger's team is not here"
        );
        assert_eq!(
            teams[0].member("grafter").unwrap().colour.as_deref(),
            Some("green")
        );
        assert!(read_project_agents(&checkout.path().join("none")).is_empty());
        // A checkout no team is led from takes nothing from its registry.
        let other = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(other.path().join(".claude")).unwrap();
        std::fs::write(
            other.path().join(".claude").join(REGISTRY_FILE),
            r#"[{"name":"lone"}]"#,
        )
        .unwrap();
        let teams = teams_for_checkout(config.path(), other.path());
        assert!(teams[0].member("lone").is_none());
    }

    #[test]
    fn oversized_files_are_left_unread() {
        let config = tempfile::tempdir().unwrap();
        let big = config.path().join(TEAMS_DIR).join("big");
        std::fs::create_dir_all(&big).unwrap();
        let file = std::fs::File::create(big.join(TEAM_FILE)).unwrap();
        file.set_len(MAX_FILE + 1).unwrap();
        assert!(read_teams(config.path()).is_empty());
    }
}
