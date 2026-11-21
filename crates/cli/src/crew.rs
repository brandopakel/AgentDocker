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
    /// Save the proposed team as this project's: the one whose key you
    /// were shown. A team proposed since, from other runtimes, is refused.
    Accept {
        /// The proposal's key, as `agentdocker team` printed it.
        #[arg(value_name = "KEY")]
        proposal: String,
    },
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

/// The project an invocation is about, as the daemon is told it: the one
/// named, a path made whole, or the folder the command runs in.
fn selected(project: Option<String>) -> String {
    crate::project_selector(project.as_deref().unwrap_or("."))
}

pub async fn run(client: &Client, args: Args) -> Result<()> {
    let asked = selected(args.project);
    let project = Some(asked.clone());
    let shown = match args.action {
        None => read(client, &project).await?,
        Some(action) => {
            // Changing the team is the person's: a command run from an
            // agent's own shell speaks as that agent, and is refused.
            let from = sender::resolve(client, None)
                .await?
                .unwrap_or_else(|| HUMAN.into());
            let request = match action {
                // What is accepted is what the person was shown: the key
                // they typed goes to the daemon as it is. The team is not
                // read again here, or a proposal made since from other
                // runtimes would be saved unseen under its own key.
                Action::Accept { proposal } => accept(from, project.clone(), proposal),
                Action::Add(add) => {
                    let now = read(client, &project).await?;
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
                    let now = read(client, &project).await?;
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
        print!("{}", text(&shown, &asked));
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

/// The request that accepts a proposal: the key the person typed, as it
/// is. The daemon holds it to the proposal there is now and refuses
/// another.
fn accept(from: String, project: Option<String>, proposal: String) -> Request {
    Request::CrewAccept {
        from,
        project,
        proposal,
    }
}

/// The command that accepts the proposal a person is looking at: its
/// key, and the project they asked about, each one shell word. It names
/// the project because the command may be run from another folder than
/// the one the team was read in; the key is this project's alone, so run
/// without the project elsewhere it is refused, never saved there. None
/// when the project cannot be written on one line.
fn accept_command(key: &str, project: &str) -> Option<String> {
    Some(format!(
        "agentdocker team accept {} --project {}",
        crate::gate::shell_word(key)?,
        crate::gate::shell_word(project)?
    ))
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

/// The team for a person to read. `asked` is the project as this
/// invocation named it to the daemon, which a command printed for the
/// person to run names again.
pub fn text(view: &CrewView, asked: &str) -> String {
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
            "Every member runs on one vendor's runtime: no member's work is reviewed on another vendor's.\n",
        );
    }
    if let Some(key) = &view.proposal {
        match accept_command(key, asked) {
            Some(command) => out.push_str(&format!(
                "Accept this one with `{command}`, or build your own with `agentdocker team add`.\n"
            )),
            None => out.push_str(&format!(
                "Accept this one with `agentdocker team accept` naming its key, `{key}`, and this project with --project, or build your own with `agentdocker team add`.\n"
            )),
        }
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
        proposed.proposal =
            Some("writer=claude-code:implementer,reviewer=codex:reviewer@project".into());
        assert_eq!(
            text(&proposed, "/work/app"),
            "No team is saved for this project. Proposed, from what is installed here:\n\
             \x20 writer     claude-code (anthropic)   implementer   own worktree\n\
             \x20 reviewer   codex (openai)            reviewer\n\
             Accept this one with `agentdocker team accept writer=claude-code:implementer,reviewer=codex:reviewer@project --project /work/app`, or build your own with `agentdocker team add`.\n"
        );

        let saved = view(2, vec![writer.clone(), reviewer.clone()]);
        assert_eq!(
            text(&saved, "/work/app"),
            "Team (revision 2):\n\
             \x20 writer     claude-code (anthropic)   implementer   own worktree, not started\n\
             \x20 reviewer   codex (openai)            reviewer      not started\n"
        );

        let mut gone = reviewer.clone();
        gone.installed = false;
        gone.vendor = None;
        let mut one = view(3, vec![writer.clone(), gone]);
        one.same_vendor = true;
        let said = text(&one, "/work/app");
        assert!(said.contains("reviewer   codex"), "{said}");
        assert!(
            said.contains("codex is not installed here, not started"),
            "{said}"
        );
        assert!(said.ends_with(
            "Every member runs on one vendor's runtime: no member's work is reviewed on another vendor's.\n"
        ));

        assert_eq!(
            text(&view(4, vec![]), "/work/app"),
            "Team (revision 4): no members. Add one with `agentdocker team add <name> --runtime <runtime>`.\n"
        );

        let mut nothing = view(0, vec![]);
        nothing.unavailable = Some("no runtime is installed".into());
        assert_eq!(
            text(&nothing, "/work/app"),
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
    }

    /// The real command tree, on a thread with room for it.
    fn parsed(words: Vec<String>) -> Result<crate::Cli, clap::Error> {
        use clap::Parser;
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(move || crate::Cli::try_parse_from(words))
            .unwrap()
            .join()
            .unwrap()
    }

    /// What is accepted is what was shown, where it was shown. The
    /// command a person is given carries the key of the proposal they are
    /// looking at and the project they asked about; typed back, from any
    /// folder, it is that key and that project the daemon is sent, and
    /// accepting names a key or is not a command at all. It used to read
    /// the team again and send whatever was proposed by then; and then to
    /// name no project, so that copied into another checkout it saved
    /// that checkout's project instead.
    #[test]
    fn accepting_sends_the_key_and_the_project_the_person_was_shown() {
        let key = "writer=claude-code:implementer,reviewer=codex:reviewer@project-b";
        let mut proposed = view(
            0,
            vec![member(
                "writer",
                "claude-code",
                Some("Anthropic"),
                &["implementer"],
            )],
        );
        proposed.proposal = Some(key.into());
        // Asked about by a path with a space and a quote in it, from
        // some other folder: the hardest thing the command must carry.
        for asked in ["/work/b", "/work/it's here/b"] {
            let said = text(&proposed, asked);
            let command = said
                .split('`')
                .find(|part| part.starts_with("agentdocker team accept "))
                .expect("the proposal is shown with the command that accepts it");
            assert_eq!(Some(command), accept_command(key, asked).as_deref());

            // The command as a shell would hand it over, through the real
            // command tree, to the request it makes: nothing here is
            // supplied by the test but who is asking.
            let words = shlex::split(command).expect("the command is shell words");
            let cli = parsed(words).expect("the command a person is given parses");
            let crate::Command::Team(args) = cli.command else {
                panic!("it is the team command");
            };
            let Some(Action::Accept { proposal }) = args.action else {
                panic!("it accepts");
            };
            assert_eq!(args.project.as_deref(), Some(asked));
            let request = accept("user".into(), Some(selected(args.project)), proposal);
            let Request::CrewAccept {
                proposal,
                from,
                project,
            } = request
            else {
                panic!("it is the accept request");
            };
            assert_eq!(
                (proposal.as_str(), from.as_str(), project.as_deref()),
                (key, "user", Some(asked))
            );
        }

        // A project that cannot be written on one line gets no command
        // that would mean something else: the key and what to name.
        let said = text(&proposed, "/work/two\nlines");
        assert!(!said.contains("--project /work"), "{said}");
        assert!(
            said.contains(&format!("naming its key, `{key}`, and this project")),
            "{said}"
        );

        // No project named: the folder the command runs in, made whole.
        assert_eq!(selected(None), crate::project_selector("."));
        assert!(std::path::Path::new(&selected(None)).is_absolute());

        // Without a key there is nothing to accept: the command does not
        // go and find one.
        let bare = parsed(
            ["agentdocker", "team", "accept"]
                .map(str::to_owned)
                .to_vec(),
        );
        assert!(bare.is_err(), "accepting names the proposal it accepts");
    }
}
