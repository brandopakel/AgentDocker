//! Compact send-time warnings belong to the draft destination they answered,
//! and every readiness the app shows is the one decision
//! (`agentdocker_core::readiness`): its words, its tone and its one fix.
use super::{
    App, Message,
    chip::{Chip, Tone},
    icons::{Icon, icon},
    messages::look::{link, status_line},
    shell::{DeliveryTarget, TextDraft},
    style::{Colors, RADIUS_MD, alpha, mix, weight},
    view::{small, tip},
};
use crate::controls::{Kind, button as action, custom_sized};
use agentdocker_core::readiness::{Fix, State};
use iced::{
    Element, Fill, Font,
    widget::{column, container, row, scrollable, text},
};

/// What a session's readiness chip says: the one decision, or one of the
/// app's own two facts about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Readiness {
    /// The daemon is not connected: nothing current to say.
    Unavailable,
    /// A ready receiver with words queued that no receipt names yet: they
    /// wait on the provider to take them, whatever an earlier receipt says.
    Sending,
    Decided(State),
}

impl Readiness {
    /// Its words: the decision's own, the same as `ps` and a send result.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Unavailable => "Readiness unavailable",
            Self::Sending => "Sent · waiting for the agent to take it",
            Self::Decided(state) => state.label(),
        }
    }

    /// Its tone, by the state, never by the words: positive while it takes
    /// messages, the accent while words wait for it or it waits for a
    /// prompt or a check-in, attention when something stands in the way,
    /// quiet when it has ended or nothing is known.
    pub(super) fn tone(self) -> Tone {
        match self {
            Self::Decided(State::Ready { .. }) => Tone::Positive,
            Self::Sending | Self::Decided(State::WaitsForPrompt | State::AwaitsCheckIn) => {
                Tone::Info
            }
            Self::Decided(
                State::ProviderBlocked(_) | State::Paused | State::Silent | State::Unreachable,
            ) => Tone::Attention,
            Self::Decided(State::Ended) | Self::Unavailable => Tone::Neutral,
        }
    }

    fn glyph(self) -> Option<Icon> {
        match self {
            Self::Decided(State::Ready { .. }) => Some(Icon::Check),
            Self::Sending => Some(Icon::ArrowUp),
            Self::Decided(State::WaitsForPrompt | State::AwaitsCheckIn) => Some(Icon::Clock),
            Self::Decided(State::ProviderBlocked(_)) => Some(Icon::Hourglass),
            Self::Decided(State::Paused) => Some(Icon::Pause),
            Self::Decided(State::Silent | State::Unreachable) => Some(Icon::Alert),
            Self::Decided(State::Ended) | Self::Unavailable => None,
        }
    }

    /// Its chip: the label, in its tone, the same on the session's panel,
    /// the composer and the tool's sessions.
    pub(super) fn chip(self, id: impl Into<String>) -> Chip {
        Chip::new(id, self.glyph(), self.label(), self.tone())
    }
}

pub(super) fn notice(
    draft: &TextDraft,
    target: DeliveryTarget,
    c: Colors,
) -> Option<Element<'static, Message>> {
    notice_content(draft, target, c, true)
}

/// A composer already scrolls all its feedback within the available pane.
pub(super) fn composer_notice(
    draft: &TextDraft,
    target: DeliveryTarget,
    c: Colors,
) -> Option<Element<'static, Message>> {
    notice_content(draft, target, c, false)
}

/// One status line — an amber dot, `Queued · 1 session needs attention`
/// and a quiet Details link — and, opened, who needs what and the two
/// things to do about each, in a quiet box under it.
fn notice_content(
    draft: &TextDraft,
    target: DeliveryTarget,
    c: Colors,
    scroll_details: bool,
) -> Option<Element<'static, Message>> {
    let report = draft.readiness.as_ref()?;
    if report.needs_attention == 0 {
        return None;
    }
    let key = match &target {
        DeliveryTarget::Conversation(key) => format!("conversation-{key}"),
        DeliveryTarget::Session(key) => format!("session-{key}"),
    };
    let expanded = draft.readiness_expanded;
    let toggle = link(
        format!("delivery-details-{key}"),
        if expanded {
            "Hide details"
        } else {
            "Delivery details"
        },
        if expanded { "Hide details" } else { "Details" },
        Some(Message::DeliveryDetails(target)),
        c.accent,
    );
    let mut body = column![status_line(
        c.amber,
        format!(
            "Queued · {} {}",
            report.needs_attention,
            if report.needs_attention == 1 {
                "session needs attention"
            } else {
                "sessions need attention"
            }
        ),
        c.amber,
        vec![toggle],
        c,
    )]
    .spacing(6);
    if expanded {
        // Who, what, and the way to the fix: the session's panel holds the
        // command or the button that does it.
        let mut details = column![].spacing(8);
        for recipient in &report.details {
            details = details.push(
                row![
                    text(format!("{} · {}", recipient.name, recipient.label()))
                        .size(13)
                        .font(weight(iced::font::Weight::Medium))
                        .width(Fill),
                    link(
                        format!("delivery-session-{key}-{}", recipient.agent),
                        format!("Open {}", recipient.name),
                        "Open session",
                        Some(Message::OpenSession(recipient.agent.to_string())),
                        c.accent,
                    ),
                ]
                .spacing(8)
                .align_y(iced::Center),
            );
        }
        if report.omitted() > 0 {
            details = details.push(small(
                format!(
                    "{} more sessions need attention. Check their Connection details in Tools.",
                    report.omitted()
                ),
                c,
            ));
        }
        let details: Element<'static, Message> = if scroll_details {
            scrollable(details).height(180).into()
        } else {
            details.into()
        };
        body = body.push(
            container(details)
                .padding([10, 12])
                .width(Fill)
                .style(move |_| container::Style {
                    background: Some(alpha(c.text, if c.dark { 0.04 } else { 0.03 }).into()),
                    border: iced::Border {
                        color: c.line,
                        width: 1.0,
                        radius: RADIUS_MD.into(),
                    },
                    ..Default::default()
                }),
        );
    }
    Some(body.into())
}

impl App {
    /// How to get a session that cannot take messages taking them again:
    /// the one fix the readiness decision names, in as few words as do it.
    /// An ended Claude Code session this window can bring back gets the
    /// **Reconnect here** button that does it; a fix that is a command
    /// (the Claude resume, the setup, the Codex resume) is the command to
    /// paste, whole, with a copy button; a Codex that waits for its prompt
    /// gets the way to a live one; anything else, one line. Never two
    /// actions for one fix.
    pub(super) fn fix_view(
        &self,
        agent: &agentdocker_core::AgentRecord,
        key: &str,
        c: Colors,
    ) -> Option<Element<'static, Message>> {
        if agent.spec.runtime == agentdocker_core::HUMAN_RUNTIME {
            return None;
        }
        let decision = self.readiness_decision(agent);
        if !decision.state.needs_attention() {
            return None;
        }
        if self.reconnects_here(agent) {
            let reconnecting = self.shell.reconnecting.as_deref() == Some(agent.id.as_str());
            return Some(action(
                format!("reconnect-{}", agent.id),
                if reconnecting {
                    "Reconnecting…"
                } else {
                    "Reconnect here"
                },
                (!reconnecting && !self.shell.launching && self.connected.is_ok())
                    .then(|| Message::Reconnect(agent.id.to_string())),
                false,
            ));
        }
        let hint = small(decision.hint(), c);
        if let Some(command) = decision.command(&agent.spec.runtime) {
            return Some(
                column![
                    command_well(format!("reconnect-copy-{key}-{}", agent.id), command, c),
                    hint
                ]
                .spacing(6)
                .into(),
            );
        }
        if decision.fix == Fix::LaunchLive
            && let Some(project) = agent.project.as_ref().map(|p| p.root.clone())
        {
            return Some(
                row![
                    hint.width(Fill),
                    action(
                        format!("reconnect-launch-{key}-{}", agent.id),
                        "Start a live Codex",
                        Some(Message::LaunchIn(project, "codex".into())),
                        false
                    ),
                ]
                .spacing(8)
                .align_y(iced::Center)
                .into(),
            );
        }
        Some(hint.into())
    }
}

/// A command to paste, the way an approval shows one: a prompt mark, the
/// command whole in the code face on a step of tone, and a copy button
/// that copies exactly it.
fn command_well(id: String, command: String, c: Colors) -> Element<'static, Message> {
    container(
        row![
            text("$")
                .size(13)
                .line_height(iced::Pixels(20.0))
                .font(Font::MONOSPACE)
                .color(c.amber),
            text(command.clone())
                .size(12)
                .line_height(iced::Pixels(20.0))
                .font(Font::MONOSPACE)
                .color(c.text)
                .width(Fill)
                .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
            tip(
                custom_sized(
                    id,
                    "Copy command",
                    container(icon(Icon::Copy, c.muted, 14.0)).center(20),
                    Some(Message::CopyGuidance(command)),
                    false,
                    Kind::Ghost,
                    [0, 0],
                    iced::Length::Shrink,
                ),
                "Copy command",
                c,
            ),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Start),
    )
    .padding([8, 10])
    .width(Fill)
    .style(move |_| container::Style {
        background: Some(mix(c.card, c.raised, 0.6).into()),
        border: iced::Border {
            radius: RADIUS_MD.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

#[cfg(test)]
mod tests {
    use super::super::view::testing::seen;
    use super::*;
    use agentdocker_core::{AgentRecord, AgentSpec, AgentStatus};

    /// An absolute checkout on every platform.
    const CHECKOUT: &str = if cfg!(windows) {
        r"C:\work\orchard"
    } else {
        "/work/orchard"
    };

    fn session(runtime: &str) -> AgentRecord {
        let mut agent = AgentRecord::new(
            AgentSpec {
                name: format!("{runtime}-1"),
                runtime: runtime.into(),
                workdir: Some(CHECKOUT.into()),
                ..Default::default()
            },
            false,
            chrono::Utc::now() - chrono::Duration::minutes(10),
        );
        agent.status = AgentStatus::Running;
        agent.project = Some(agentdocker_core::ProjectRef::directory(CHECKOUT));
        agent
            .spec
            .labels
            .insert("session_id".into(), "1a2b3c4d-5e6f".into());
        agent
    }

    fn app(agents: Vec<AgentRecord>) -> App {
        let (tx, _commands) = super::super::queue::channel();
        let (_messages, rx) = std::sync::mpsc::sync_channel(super::super::MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        app.connected = Ok(());
        app.agents = agents;
        app
    }

    fn claude_installed(app: &mut App) {
        app.runtimes = vec![agentdocker_core::runtime::RuntimeInfo {
            name: "claude-code".into(),
            vendor: "fixture".into(),
            label: "Claude Code".into(),
            cli: Some("/fixture/claude".into()),
            version: None,
            apps: vec![],
            extensions: vec![],
            incomplete: vec![],
            config_dir: None,
            mcp: agentdocker_core::runtime::Wiring::Wired,
            hooks: agentdocker_core::runtime::Wiring::Wired,
            hooks_missing: vec![],
            shell: agentdocker_core::runtime::Wiring::Unsupported,
            running: 0,
        }];
    }

    /// A live Claude session that cannot be woken shows the one command to
    /// paste, whole, with a copy button, and one short line; no paragraph
    /// of instructions and no second action.
    #[test]
    fn a_claude_session_shows_its_command_and_a_copy_button() {
        let agent = session("claude-code");
        let mut app = app(vec![agent.clone()]);
        claude_installed(&mut app);
        let c = Colors::new(false);
        let view = seen(app.fix_view(&agent, "inspector", c).unwrap());
        let command =
            agentdocker_core::send_readiness::claude_resume_command(&agent, cfg!(windows)).unwrap();
        assert!(view.reads(&command), "the whole command, as it is copied");
        let copy = view
            .control(&format!("reconnect-copy-inspector-{}", agent.id))
            .expect("copy");
        assert_eq!((copy.1.as_str(), copy.3), ("Copy command", true));
        assert!(view.reads("Quit it, then run this in a terminal."));
        assert!(!view.reads("Copy instructions") && !view.reads("After saving work"));
        assert!(
            view.control(&format!("reconnect-{}", agent.id)).is_none(),
            "one action: the command, not a button that waits too"
        );
    }

    /// Ended, the window brings it back itself: **Reconnect here** is the
    /// one action, with no command beside it. Where the window cannot (no
    /// Claude Code here), the command is.
    #[test]
    fn an_ended_claude_session_has_one_action_reconnect_here() {
        let mut agent = session("claude-code");
        agent.status = AgentStatus::Exited { code: Some(0) };
        let mut app = app(vec![agent.clone()]);
        claude_installed(&mut app);
        let c = Colors::new(false);
        let view = seen(app.fix_view(&agent, "inspector", c).unwrap());
        let button = view
            .control(&format!("reconnect-{}", agent.id))
            .expect("Reconnect here");
        assert_eq!((button.1.as_str(), button.3), ("Reconnect here", true));
        assert!(
            view.controls
                .iter()
                .all(|c| !c.0.starts_with("reconnect-copy")),
            "no command beside the button"
        );
        app.runtimes.clear();
        let view = seen(app.fix_view(&agent, "inspector", c).unwrap());
        assert!(view.control(&format!("reconnect-{}", agent.id)).is_none());
        assert!(view.reads("Run this in a terminal to bring it back."));
        // One AgentDocker supervised comes back through the daemon, never
        // a plain resume that would leave its queue behind: the button
        // here, and `agentdocker reconnect` where the window cannot.
        let mut managed = agent.clone();
        managed.managed = true;
        managed.spec.tty = true;
        app.agents = vec![managed.clone()];
        let view = seen(app.fix_view(&managed, "inspector", c).unwrap());
        assert!(view.reads(&format!("agentdocker reconnect {}", managed.id)));
        assert!(!view.reads("claude --resume"));
        claude_installed(&mut app);
        let view = seen(app.fix_view(&managed, "inspector", c).unwrap());
        assert!(view.control(&format!("reconnect-{}", managed.id)).is_some());
    }

    /// A Claude session nothing of which reached AgentDocker is set up
    /// first: the resume alone would fail without the MCP entry.
    #[test]
    fn an_unreachable_claude_session_is_set_up_first() {
        let mut agent = session("claude-code");
        agent.process_started_at = agent.started_at.or(Some(agent.created_at));
        let app = app(vec![agent.clone()]);
        assert_eq!(
            app.readiness_decision(&agent).state,
            agentdocker_core::readiness::State::Unreachable
        );
        let view = seen(app.fix_view(&agent, "tools", Colors::new(false)).unwrap());
        assert!(view.reads("agentdocker setup claude-code"));
        assert!(view.reads("Run this, then restart it."));
        assert!(
            !view.reads("claude --resume"),
            "one fix, not the resume too"
        );
    }

    /// A Codex session says when it takes messages and offers the session
    /// that takes them live; nothing to copy that would not work.
    #[test]
    fn a_codex_session_offers_a_live_one_instead_of_instructions() {
        let agent = session("codex");
        let app = app(vec![agent.clone()]);
        let view = seen(
            app.fix_view(&agent, "inspector", Colors::new(true))
                .unwrap(),
        );
        assert!(view.reads("It takes messages at its next prompt."));
        assert_eq!(
            view.control(&format!("reconnect-launch-inspector-{}", agent.id))
                .unwrap()
                .1,
            "Start a live Codex"
        );
        assert!(
            view.controls
                .iter()
                .all(|c| !c.0.starts_with("reconnect-copy")),
            "no copy"
        );
    }

    /// The panels, the composer's chip and a send's details render the one
    /// decision: the same label, the same short line for its fix, and the
    /// tone by its state, whatever the words.
    #[test]
    fn every_surface_of_the_app_renders_the_one_decision() {
        use agentdocker_core::readiness::State;
        let mut browser = session("claude-browser");
        browser.process_started_at = None;
        browser.spec.labels.insert(
            agentdocker_core::agent::CONNECTOR_LABEL.into(),
            "true".into(),
        );
        let mut unreachable = session("codex");
        unreachable.process_started_at = Some(unreachable.created_at);
        let agents = vec![
            session("claude-code"),
            session("codex"),
            session("custom"),
            browser,
            unreachable,
        ];
        let app = app(agents.clone());
        let c = Colors::new(false);
        for agent in &agents {
            let decision = app.readiness_decision(agent);
            assert_eq!(
                app.input_readiness(agent),
                Readiness::Decided(decision.state)
            );
            let readiness =
                agentdocker_core::RecipientReadiness::for_agent(agent, chrono::Utc::now(), None)
                    .unwrap();
            assert_eq!(readiness.label(), decision.state.label());
            assert_eq!(readiness.fix.as_ref(), Some(&decision.fix));
            let view = seen(app.fix_view(agent, "tools", c).unwrap());
            assert!(view.reads(decision.hint()), "{}", agent.spec.name);
            if let Some(command) = decision.command(&agent.spec.runtime) {
                assert!(view.reads(&command), "{}", agent.spec.name);
            }
        }
        assert_eq!(
            app.input_readiness(&agents[3]).label(),
            "Waits until it checks in",
            "a browser agent, in the words its messages say"
        );
        assert_eq!(
            Readiness::Decided(State::AwaitsCheckIn).tone(),
            Readiness::Decided(State::WaitsForPrompt).tone()
        );
        assert_eq!(
            Readiness::Decided(State::Unreachable).tone(),
            Tone::Attention
        );
        let receiving = Readiness::Decided(State::Ready { receiving: true }).chip("r");
        assert_eq!(
            (receiving.words.as_str(), receiving.tone),
            ("Receiving messages", Tone::Positive)
        );
        // The chip is the decision's own words, named by them.
        for agent in &agents {
            let readiness = app.input_readiness(agent);
            let view = seen(readiness.chip("readiness").view(c));
            assert_eq!(
                view.control("readiness").unwrap().1,
                readiness.label(),
                "{}",
                agent.spec.name
            );
        }
    }

    /// Start a live Codex opens the launch form in the session's own
    /// project, not the one on view (Tools lists every project's sessions).
    #[test]
    fn a_live_codex_starts_in_the_sessions_own_project() {
        let mut app = app(Vec::new());
        for root in ["/work/a", "/work/b"] {
            app.shell.catalog.projects.push(crate::catalog::Entry {
                project: agentdocker_core::ProjectRef::directory(root),
                pinned: true,
                label: None,
            });
        }
        app.shell.catalog.selected = Some("/work/a".into());
        let _ = app.update(Message::LaunchIn("/work/b".into(), "codex".into()));
        assert_eq!(
            app.shell.catalog.selected.as_deref(),
            Some(std::path::Path::new("/work/b"))
        );
        assert!(app.shell.launch);
        assert_eq!(app.shell.launch_runtime.as_deref(), Some("codex"));
        // A project not in the list is not launched into the one on view.
        app.shell.launch = false;
        let _ = app.update(Message::LaunchIn("/work/c".into(), "codex".into()));
        assert!(!app.shell.launch);
        assert_eq!(
            app.shell.catalog.selected.as_deref(),
            Some(std::path::Path::new("/work/b"))
        );
    }

    /// Delivery details are one line per session and the way to it, in the
    /// decision's words.
    #[test]
    fn delivery_details_are_one_line_per_session() {
        let agent = session("claude-code");
        let mut report = agentdocker_core::SendReadiness::default();
        report.observe(agentdocker_core::RecipientReadiness::for_agent(
            &agent,
            chrono::Utc::now(),
            None,
        ));
        let mut draft = TextDraft::default();
        draft.readiness = Some(report);
        draft.readiness_expanded = true;
        let view = seen(
            notice(
                &draft,
                DeliveryTarget::Session("s".into()),
                Colors::new(false),
            )
            .unwrap(),
        );
        assert!(view.reads("claude-code-1 · Messages may wait for its next prompt"));
        assert!(
            view.control(&format!("delivery-session-session-s-{}", agent.id))
                .is_some()
        );
        assert!(!view.reads("Copy instructions") && !view.reads("After saving work"));
    }
}
