//! `agentdocker team`: a project's team, read and edited.
//!
//! The daemon keeps the team's description (its *crew*); this reads it
//! and saves the person's edits. With none saved it shows the team the
//! daemon proposes from what is installed. Nothing here starts a session.
use crate::client::Client;
use crate::sender;
use agentdocker_core::crew::{CrewView, MemberInput, MemberView};
use agentdocker_core::{HUMAN, Request, Response};
use anyhow::{Result, bail};

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    action: Option<Action>,
    /// The project (default: the one this folder is in).
    #[arg(long, value_name = "ID|PATH", global = true)]
    project: Option<String>,
    /// Print the team as JSON.
    #[arg(long, global = true)]
    json: bool,
}

#[derive(clap::Subcommand)]
enum Action {
    /// Save the proposed team as this project's.
    Accept,
    /// Add a member to the team.
    Add(AddArgs),
    /// Take a member out of the team, by its name.
    #[command(alias = "remove")]
    Rm { name: String },
}

#[derive(clap::Args)]
struct AddArgs {
    /// What the member is called: lower-case letters, digits and hyphens.
    name: String,
    /// claude-code or codex.
    #[arg(long)]
    runtime: String,
    /// A role of the project's palette its session is given (repeatable).
    #[arg(long = "role", value_name = "ROLE")]
    roles: Vec<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    effort: Option<String>,
    /// Its session works in a worktree of its own.
    #[arg(long)]
    own_worktree: bool,
    /// A plan its first words name.
    #[arg(long, value_name = "PLAN")]
    plan: Option<String>,
    /// What its session is told first, in place of the default for its roles.
    #[arg(long, value_name = "TEXT")]
    first_words: Option<String>,
}

async fn read(client: &Client, project: &Option<String>) -> Result<CrewView> {
    match client
        .call(&Request::Crew {
            project: project.clone(),
        })
        .await?
    {
        Response::Crew { crew } => Ok(crew),
        other => bail!("the daemon answered the team with {other:?}"),
    }
}

fn saved(response: Response) -> Result<CrewView> {
    match response {
        Response::Crew { crew } => Ok(crew),
        other => bail!("the daemon answered the save with {other:?}"),
    }
}

pub async fn run(client: &Client, args: Args) -> Result<()> {
    let project = Some(match args.project {
        Some(project) => project,
        None => std::env::current_dir()?.display().to_string(),
    });
    let shown = match args.action {
        None => read(client, &project).await?,
        Some(action) => {
            // Changing the team is the person's: a command run from an
            // agent's own shell speaks as that agent, and is refused.
            let from = sender::resolve(client, None)
                .await?
                .unwrap_or_else(|| HUMAN.into());
            let now = read(client, &project).await?;
            let request = match action {
                Action::Accept => {
                    let Some(proposal) = now.proposal.clone() else {
                        bail!("{}", nothing_to_accept(&now));
                    };
                    Request::CrewAccept {
                        from,
                        project: project.clone(),
                        proposal,
                    }
                }
                Action::Add(add) => {
                    let mut members = kept(&now);
                    members.push(MemberInput {
                        id: None,
                        name: add.name,
                        runtime: add.runtime,
                        model: add.model,
                        effort: add.effort,
                        roles: add.roles,
                        own_worktree: add.own_worktree,
                        plan: add.plan,
                        first_words: add.first_words,
                    });
                    Request::CrewSave {
                        from,
                        project: project.clone(),
                        expect: now.revision,
                        members,
                    }
                }
                Action::Rm { name } => {
                    let members = kept(&now);
                    if now.revision == 0 || !members.iter().any(|m| m.name == name) {
                        bail!("{}", no_such_member(&now, &name));
                    }
                    Request::CrewSave {
                        from,
                        project: project.clone(),
                        expect: now.revision,
                        members: members.into_iter().filter(|m| m.name != name).collect(),
                    }
                }
            };
            saved(client.call(&request).await?)?
        }
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&shown)?);
    } else {
        print!("{}", text(&shown));
    }
    Ok(())
}

/// The saved team's members as an edit gives them back; none while what is
/// shown is a proposal, which is not the project's until it is accepted.
fn kept(view: &CrewView) -> Vec<MemberInput> {
    if view.revision == 0 {
        return Vec::new();
    }
    view.members.iter().map(MemberView::input).collect()
}

fn nothing_to_accept(view: &CrewView) -> String {
    match (&view.unavailable, view.revision) {
        (Some(why), _) => format!("there is no team to accept: {why}"),
        (None, revision) => format!(
            "the project has a team saved (revision {revision}); change it with `agentdocker team add` and `agentdocker team rm`"
        ),
    }
}

fn no_such_member(view: &CrewView, name: &str) -> String {
    if view.revision == 0 {
        return "no team is saved for this project; nothing to take a member out of".to_owned();
    }
    let names: Vec<&str> = view.members.iter().map(|m| m.name.as_str()).collect();
    if names.is_empty() {
        format!("the team has no member called `{name}`; it has no members")
    } else {
        format!(
            "the team has no member called `{name}`; its members are {}",
            names.join(", ")
        )
    }
}

/// The team for a person to read.
pub fn text(view: &CrewView) -> String {
    let mut out = String::new();
    if let Some(why) = &view.unavailable {
        out.push_str(&format!(
            "No team is saved for this project, and none can be proposed: {why}.\n"
        ));
        return out;
    }
    match (&view.proposal, view.members.is_empty()) {
        (Some(_), _) => out.push_str(
            "No team is saved for this project. Proposed, from what is installed here:\n",
        ),
        (None, true) => {
            out.push_str(&format!(
                "Team (revision {}): no members. Add one with `agentdocker team add <name> --runtime <runtime>`.\n",
                view.revision
            ));
            return out;
        }
        (None, false) => out.push_str(&format!("Team (revision {}):\n", view.revision)),
    }
    let cell = |m: &MemberView| match &m.vendor {
        Some(vendor) => format!("{} ({vendor})", m.runtime),
        None => m.runtime.clone(),
    };
    let width = |f: &dyn Fn(&MemberView) -> usize| view.members.iter().map(f).max().unwrap_or(0);
    let (names, runtimes, roles) = (
        width(&|m| m.name.chars().count()),
        width(&|m| cell(m).chars().count()),
        width(&|m| m.roles.join(", ").chars().count()),
    );
    for member in &view.members {
        let mut notes = Vec::new();
        if member.own_worktree {
            notes.push("own worktree".to_owned());
        }
        if !member.installed {
            notes.push(format!("{} is not installed here", member.runtime));
        }
        if view.proposal.is_none() {
            notes.push(member.standing.word().to_owned());
        }
        let line = format!(
            "  {:<names$}   {:<runtimes$}   {:<roles$}   {}",
            member.name,
            cell(member),
            member.roles.join(", "),
            notes.join(", "),
        );
        out.push_str(line.trim_end());
        out.push('\n');
    }
    if view.same_vendor {
        out.push_str(
            "Every member runs on one vendor: nobody's work is reviewed by another vendor's model.\n",
        );
    }
    if view.proposal.is_some() {
        out.push_str(
            "Accept it with `agentdocker team accept`, or build your own with `agentdocker team add`.\n",
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::ProjectId;
    use agentdocker_core::crew::Standing;

    fn member(name: &str, runtime: &str, vendor: Option<&str>, roles: &[&str]) -> MemberView {
        MemberView {
            id: Some(format!("id-{name}").as_str().into()),
            name: name.into(),
            runtime: runtime.into(),
            vendor: vendor.map(str::to_owned),
            installed: true,
            model: None,
            effort: None,
            roles: roles.iter().map(|r| (*r).to_owned()).collect(),
            own_worktree: false,
            plan: None,
            first_words: None,
            standing: Standing::NotStarted,
        }
    }

    fn view(revision: u64, members: Vec<MemberView>) -> CrewView {
        CrewView {
            project: ProjectId::from("project"),
            revision,
            proposal: None,
            unavailable: None,
            members,
            same_vendor: false,
        }
    }

    /// What a person reads in each case: a proposal with how to take it,
    /// a saved team with where each member stands, a team on one vendor
    /// said to be one, a member whose runtime has gone, an emptied team,
    /// and no team with why.
    #[test]
    fn the_team_is_said_in_each_of_its_cases() {
        let mut writer = member("writer", "claude-code", Some("anthropic"), &["implementer"]);
        writer.own_worktree = true;
        let reviewer = member("reviewer", "codex", Some("openai"), &["reviewer"]);

        let mut proposed = view(0, vec![writer.clone(), reviewer.clone()]);
        proposed.proposal = Some("writer=claude-code:implementer,reviewer=codex:reviewer".into());
        assert_eq!(
            text(&proposed),
            "No team is saved for this project. Proposed, from what is installed here:\n\
             \x20 writer     claude-code (anthropic)   implementer   own worktree\n\
             \x20 reviewer   codex (openai)            reviewer\n\
             Accept it with `agentdocker team accept`, or build your own with `agentdocker team add`.\n"
        );

        let saved = view(2, vec![writer.clone(), reviewer.clone()]);
        assert_eq!(
            text(&saved),
            "Team (revision 2):\n\
             \x20 writer     claude-code (anthropic)   implementer   own worktree, not started\n\
             \x20 reviewer   codex (openai)            reviewer      not started\n"
        );

        let mut gone = reviewer.clone();
        gone.installed = false;
        gone.vendor = None;
        let mut one = view(3, vec![writer.clone(), gone]);
        one.same_vendor = true;
        let said = text(&one);
        assert!(said.contains("reviewer   codex"), "{said}");
        assert!(
            said.contains("codex is not installed here, not started"),
            "{said}"
        );
        assert!(said.ends_with(
            "Every member runs on one vendor: nobody's work is reviewed by another vendor's model.\n"
        ));

        assert_eq!(
            text(&view(4, vec![])),
            "Team (revision 4): no members. Add one with `agentdocker team add <name> --runtime <runtime>`.\n"
        );

        let mut nothing = view(0, vec![]);
        nothing.unavailable = Some("no runtime is installed".into());
        assert_eq!(
            text(&nothing),
            "No team is saved for this project, and none can be proposed: no runtime is installed.\n"
        );
    }

    /// An edit starts from the saved team and never from a proposal, and
    /// the refusals that need no daemon say what there is.
    #[test]
    fn an_edit_starts_from_what_is_saved() {
        let writer = member("writer", "claude-code", Some("anthropic"), &["implementer"]);
        let mut proposed = view(0, vec![writer.clone()]);
        proposed.proposal = Some("key".into());
        assert!(
            kept(&proposed).is_empty(),
            "a proposal is not the project's"
        );
        let saved = view(1, vec![writer]);
        let members = kept(&saved);
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].id.as_ref().unwrap().as_str(), "id-writer");

        assert!(no_such_member(&proposed, "writer").starts_with("no team is saved"));
        assert_eq!(
            no_such_member(&saved, "checker"),
            "the team has no member called `checker`; its members are writer"
        );
        assert!(nothing_to_accept(&saved).contains("has a team saved (revision 1)"));
        let mut nothing = view(0, vec![]);
        nothing.unavailable = Some("no runtime is installed".into());
        assert_eq!(
            nothing_to_accept(&nothing),
            "there is no team to accept: no runtime is installed"
        );
    }
}
