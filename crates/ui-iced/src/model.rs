//! Fictional workspace data and the interaction state for design discussion.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Projects,
    Inbox,
    Connections,
    Settings,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Detail {
    #[default]
    Overview,
    Activity,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Working,
    NeedsInput,
    Unknown,
}

impl Activity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::NeedsInput => "Needs input",
            Self::Unknown => "Activity unknown",
        }
    }
}

#[derive(Clone)]
pub struct Agent {
    pub name: &'static str,
    pub provider: &'static str,
    pub project: usize,
    pub branch: &'static str,
    pub activity: Activity,
    pub observation: &'static str,
    pub terminal: bool,
}

#[derive(Clone)]
pub struct Project {
    pub name: &'static str,
    pub description: &'static str,
    pub path: &'static str,
}

#[derive(Clone)]
pub struct Workspace {
    pub page: Page,
    pub project: usize,
    pub selected: Option<usize>,
    pub detail: Detail,
    pub search: String,
    pub answer: String,
    pub answered: bool,
    pub dark: bool,
    pub offline: bool,
    pub empty: bool,
    pub connection: Option<usize>,
    pub projects: Vec<Project>,
    pub agents: Vec<Agent>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            page: Page::Projects,
            project: 0,
            selected: None,
            detail: Detail::Overview,
            search: String::new(),
            answer: String::new(),
            answered: false,
            dark: false,
            offline: false,
            empty: false,
            connection: None,
            projects: vec![
                Project {
                    name: "AgentDocker",
                    description: "A quieter home for your agents.",
                    path: "~/Projects/AgentDocker",
                },
                Project {
                    name: "Example API",
                    description: "Service and client development.",
                    path: "~/Projects/example-api",
                },
                Project {
                    name: "Documentation",
                    description: "Guides, examples, and release notes.",
                    path: "~/Projects/docs",
                },
            ],
            agents: vec![
                Agent {
                    name: "Desktop cleanup",
                    provider: "Claude Code",
                    project: 0,
                    branch: "feat/desktop",
                    activity: Activity::Working,
                    observation: "Tool activity reported 12 seconds ago",
                    terminal: true,
                },
                Agent {
                    name: "Release checks",
                    provider: "Codex CLI",
                    project: 0,
                    branch: "fix/packaging",
                    activity: Activity::Working,
                    observation: "Tool activity reported 28 seconds ago",
                    terminal: true,
                },
                Agent {
                    name: "Design review",
                    provider: "Claude Code",
                    project: 0,
                    branch: "feat/desktop",
                    activity: Activity::NeedsInput,
                    observation: "A question is waiting for you",
                    terminal: false,
                },
                Agent {
                    name: "Client library",
                    provider: "Codex CLI",
                    project: 1,
                    branch: "feat/client",
                    activity: Activity::Unknown,
                    observation: "Process found; no recent activity report",
                    terminal: false,
                },
            ],
        }
    }
}

impl Workspace {
    pub fn visible_agents(&self) -> impl Iterator<Item = (usize, &Agent)> {
        let needle = self.search.to_lowercase();
        self.agents.iter().enumerate().filter(move |(_, agent)| {
            !self.empty
                && agent.project == self.project
                && format!("{} {} {}", agent.name, agent.provider, agent.branch)
                    .to_lowercase()
                    .contains(&needle)
        })
    }

    pub fn question_pending(&self) -> bool {
        !self.empty && !self.answered
    }

    pub fn select_project(&mut self, project: usize) {
        if project < self.projects.len() {
            self.project = project;
            self.page = Page::Projects;
            self.selected = None;
            self.search.clear();
        }
    }

    pub fn send_answer(&mut self) {
        if !self.offline && self.question_pending() && !self.answer.trim().is_empty() {
            self.answered = true;
            // Answering does not prove the agent resumed work.
            self.agents[2].activity = Activity::Unknown;
            self.agents[2].observation = "Answer recorded in preview; no newer activity report";
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_projects_clears_stale_selection_without_losing_an_answer_draft() {
        let mut state = Workspace {
            selected: Some(0),
            answer: "Keep the project context".into(),
            ..Default::default()
        };
        state.select_project(1);
        assert_eq!(state.selected, None);
        assert_eq!(
            state
                .visible_agents()
                .map(|(_, a)| a.name)
                .collect::<Vec<_>>(),
            ["Client library"]
        );
        assert_eq!(state.answer, "Keep the project context");
    }

    #[test]
    fn disconnect_preserves_drafts_and_answering_does_not_invent_work() {
        let mut state = Workspace {
            offline: true,
            answer: "Open the last project".into(),
            ..Default::default()
        };
        state.send_answer();
        assert!(!state.answered);
        assert!(!state.answer.is_empty());
        state.offline = false;
        state.send_answer();
        assert!(state.answered);
        assert_eq!(state.agents[2].activity, Activity::Unknown);
    }
}
