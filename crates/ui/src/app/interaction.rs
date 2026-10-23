//! Pointer and keyboard interaction, driven through the whole window the
//! way Iced's event loop drives it (see `view::testing::Window`): where
//! the focus lands, and whether it shows a ring, after a click that also
//! changes what is on screen.
use super::view::testing::{Focused, Window};
use super::*;
use agentdocker_core::{AgentSpec, AgentStatus, ProjectRef};

fn app() -> App {
    let (commands, _requests) = queue::channel();
    let (_messages, results) = std::sync::mpsc::sync_channel(MESSAGE_CAPACITY);
    let mut app = App::bare(commands, results);
    app.connected = Ok(());
    app.shell.width = 1400.0;
    app.shell.height = 900.0;
    app.screen = Screen::Agents;
    app
}

fn session(app: &mut App, name: &str, root: &str) -> String {
    let mut agent = AgentRecord::new(
        AgentSpec {
            name: name.into(),
            runtime: "claude-code".into(),
            ..Default::default()
        },
        false,
        Utc::now(),
    );
    agent.status = AgentStatus::Running;
    agent.project = Some(ProjectRef::directory(root));
    let id = agent.id.to_string();
    app.agents.push(agent);
    id
}

fn window(app: &App) -> Window {
    let mut window = Window::new(
        app.shell.width / app.scale_factor(),
        app.shell.height / app.scale_factor(),
    );
    window.show(app);
    window
}

fn centre(bounds: iced::Rectangle) -> iced::Point {
    bounds.center()
}

/// A session row in All projects, under its project's heading, opens the
/// session when clicked: the project's page, the same row now selected in
/// a list with no headings and the details beside it. The row lands in
/// fresh widget state there, and the click's focus is applied after the
/// view is rebuilt. However the click comes — a tap whose press and
/// release reach the window together, or a press and a release a frame
/// apart — the row is focused without a ring.
#[test]
fn a_clicked_session_row_that_moves_is_focused_without_a_ring() {
    for tap in [true, false] {
        let mut app = app();
        session(&mut app, "kestrel", "/work/minio-lab");
        session(&mut app, "heron", "/work/keel");
        let id = session(&mut app, "osprey", "/work/minio-lab");
        assert!(app.all_projects());
        let mut window = window(&app);
        let row = window
            .bounds(&app, &format!("session-{id}"))
            .expect("the row in All projects");
        let heard = if tap {
            window.tap(&mut app, centre(row))
        } else {
            window.click(&mut app, centre(row))
        };
        assert!(
            heard.iter().any(|m| m.starts_with("OpenSession")),
            "the click opened the session: {heard:?}"
        );
        assert!(!app.all_projects(), "the project's page is on view");
        assert_eq!(app.shell.selected.as_deref(), Some(id.as_str()));
        let focused = window.focused(&app);
        assert!(
            focused.iter().all(|f| !f.ringed),
            "tap={tap}: a click leaves no ring: {focused:?}"
        );
    }
}

/// The same in a project's own list: selecting a session puts its
/// details beside the list, which moves the list in the widget tree.
#[test]
fn selecting_a_session_in_a_project_leaves_no_ring() {
    let mut app = app();
    let id = session(&mut app, "kestrel", "/work/minio-lab");
    session(&mut app, "heron", "/work/minio-lab");
    app.shell
        .catalog
        .remember(ProjectRef::directory("/work/minio-lab"), false)
        .unwrap();
    let _ = app.update(Message::SelectProject("/work/minio-lab".into()));
    app.screen = Screen::Agents;
    assert!(!app.all_projects());
    let mut window = window(&app);
    let row = window
        .bounds(&app, &format!("session-{id}"))
        .expect("the row");
    window.tap(&mut app, centre(row));
    assert_eq!(app.shell.selected.as_deref(), Some(id.as_str()));
    let focused = window.focused(&app);
    assert_eq!(
        focused,
        [Focused {
            id: format!("session-{id}"),
            ringed: false
        }],
        "the clicked row has the focus, without a ring"
    );
}

/// A click lands while the list changes under it: a session that has just
/// started takes the top row, and every row below moves down one. The
/// clicked row's focus is applied to the row's new place, whose widget
/// state was the row above's until this frame; it is still the pointer's
/// focus, with no ring.
#[test]
fn a_row_clicked_while_a_new_session_pushes_it_down_shows_no_ring() {
    let mut app = app();
    let root = "/work/minio-lab";
    let first = session(&mut app, "kestrel", root);
    let clicked = session(&mut app, "heron", root);
    session(&mut app, "osprey", root);
    app.shell
        .catalog
        .remember(ProjectRef::directory(root), false)
        .unwrap();
    let _ = app.update(Message::SelectProject(root.into()));
    app.screen = Screen::Agents;
    // The details are open already, so selecting another session keeps
    // the list where it is in the tree.
    let _ = app.update(Message::SelectSession(first));
    let mut window = window(&app);
    let row = window
        .bounds(&app, &format!("session-{clicked}"))
        .expect("the row");
    let messages = window.dispatch(&app, &super::view::testing::tap(centre(row)), {
        iced::mouse::Cursor::Available(centre(row))
    });
    // The same pass of the event loop brings the new session.
    let mut newest = AgentRecord::new(
        AgentSpec {
            name: "plover".into(),
            runtime: "codex".into(),
            ..Default::default()
        },
        false,
        Utc::now() + chrono::Duration::seconds(5),
    );
    newest.status = AgentStatus::Running;
    newest.project = Some(ProjectRef::directory(root));
    app.agents.push(newest);
    window.deliver(&mut app, messages);
    assert_eq!(app.shell.selected.as_deref(), Some(clicked.as_str()));
    assert_eq!(
        window.focused(&app),
        [Focused {
            id: format!("session-{clicked}"),
            ringed: false
        }],
    );
}

fn project_page(app: &mut App, root: &str) {
    app.shell
        .catalog
        .remember(ProjectRef::directory(root), false)
        .unwrap();
    let _ = app.update(Message::SelectProject(root.into()));
    app.screen = Screen::Agents;
}

fn named(key: iced::keyboard::key::Named) -> iced::keyboard::Key {
    iced::keyboard::Key::Named(key)
}

/// A menu opened from the keyboard takes the focus to its first entry,
/// and keeps it there through the frames that follow; Escape closes it
/// and gives the focus, ringed, back to the control that opened it.
#[test]
fn escape_gives_a_keyboard_opened_menu_back_to_its_opener() {
    use iced::keyboard::key::Named;
    let mut app = app();
    project_page(&mut app, "/work/keel");
    let mut window = window(&app);
    window.deliver(&mut app, vec![Message::Focus("project-more".into())]);
    // Any key the app hears ends a pointer's claim on the trigger.
    window.key(&mut app, named(Named::Shift));
    assert_eq!(app.shell.pressed, None);
    window.key(&mut app, named(Named::Enter));
    assert!(app.shell.more, "Enter opened the menu");
    let ringed = |id: &str| {
        [Focused {
            id: id.into(),
            ringed: true,
        }]
    };
    assert_eq!(window.focused(&app), ringed("project-tab-Journal"));
    window.show(&app);
    assert_eq!(
        window.focused(&app),
        ringed("project-tab-Journal"),
        "the entry keeps the focus once the view is built again"
    );
    window.key(&mut app, named(Named::Escape));
    assert!(!app.shell.more, "Escape closed it");
    assert_eq!(window.focused(&app), ringed("project-more"));
}

/// Opened by the pointer, a menu leaves the focus with its trigger,
/// quietly; Escape or a press elsewhere closes it with no ring anywhere.
#[test]
fn a_pointer_opened_menu_closes_leaving_no_ring() {
    use iced::keyboard::key::Named;
    for escape in [true, false] {
        let mut app = app();
        project_page(&mut app, "/work/keel");
        let mut window = window(&app);
        let more = window.bounds(&app, "project-more").expect("the More tab");
        window.tap(&mut app, more.center());
        assert!(app.shell.more);
        if escape {
            window.key(&mut app, named(Named::Escape));
        } else {
            // The page's own margin: nothing to press there.
            window.tap(&mut app, iced::Point::new(1395.0, 895.0));
        }
        assert!(!app.shell.more, "escape={escape}: closed");
        let focused = window.focused(&app);
        assert!(
            focused.iter().all(|f| !f.ringed),
            "escape={escape}: {focused:?}"
        );
        assert_eq!(app.shell.menu_opener, None);
    }
}

/// A menu the keyboard opened and the pointer closed, by a press on
/// nothing, does not hand the focus back: the pointer has moved on.
#[test]
fn a_keyboard_opened_menu_closed_by_the_pointer_leaves_no_ring() {
    use iced::keyboard::key::Named;
    let mut app = app();
    project_page(&mut app, "/work/keel");
    let mut window = window(&app);
    window.deliver(&mut app, vec![Message::Focus("project-more".into())]);
    window.key(&mut app, named(Named::Shift));
    window.key(&mut app, named(Named::Enter));
    assert!(app.shell.more);
    window.tap(&mut app, iced::Point::new(1395.0, 895.0));
    assert!(!app.shell.more);
    let focused = window.focused(&app);
    assert!(focused.iter().all(|f| !f.ringed), "{focused:?}");
}

/// A click in a field's words puts the caret there, and typing goes in
/// there: the focus the click asks for does not move the caret to the end
/// of the draft.
#[test]
fn a_click_in_a_field_keeps_the_caret_where_it_was_put() {
    let mut app = app();
    session(&mut app, "kestrel", "/work/keel");
    app.shell.search = "hello world".into();
    let mut window = window(&app);
    let field = window.bounds(&app, "session-search").expect("the search");
    // Just inside the field's left padding: before the first letter.
    window.tap(&mut app, iced::Point::new(field.x + 11.0, field.center_y()));
    window.typed(&mut app, "X");
    assert_eq!(app.shell.search, "Xhello world");
}

/// The launch dialog takes the focus into its task field as it opens, so
/// nothing under its scrim keeps the focus — or answers Enter and Space
/// typed into the dialog.
#[test]
fn the_launch_dialog_takes_the_focus_from_the_page_under_it() {
    let mut app = app();
    session(&mut app, "kestrel", "/work/keel");
    project_page(&mut app, "/work/keel");
    let mut window = window(&app);
    let filter = window
        .bounds(&app, "sessions-attention")
        .expect("the Needs input filter");
    window.tap(&mut app, filter.center());
    assert_eq!(
        window.focused(&app),
        [Focused {
            id: "sessions-attention".into(),
            ringed: false
        }],
    );
    window.deliver(&mut app, vec![Message::ShowLaunch]);
    assert!(app.shell.launch);
    assert_eq!(window.focused(&app), [], "no control under the scrim");
    window.typed(&mut app, "x");
    assert_eq!(app.shell.launch_prompt, "x", "typing goes to the task");
}

/// A press on a row that the list's edge cuts in half focuses it where it
/// is: the list does not scroll under the pointer between the press and
/// the release, which would move the row away from the click.
#[test]
fn a_press_on_a_half_hidden_row_does_not_scroll_the_list() {
    let mut app = app();
    let ids: Vec<String> = (0..40)
        .map(|n| session(&mut app, &format!("worker-{n}"), "/work/keel"))
        .collect();
    project_page(&mut app, "/work/keel");
    let mut window = window(&app);
    let bottom = app.shell.height / app.scale_factor() - 24.0;
    let (id, row) = ids
        .iter()
        .find_map(|id| {
            let row = window.bounds(&app, &format!("session-{id}"))?;
            (row.y < bottom - 4.0 && row.y + row.height > bottom + 4.0).then(|| (id.clone(), row))
        })
        .expect("a row cut by the list's edge");
    let before = window.scrolled(&app, "workspace-scroll");
    let heard = window.click(&mut app, iced::Point::new(row.center_x(), row.y + 2.0));
    assert_eq!(window.scrolled(&app, "workspace-scroll"), before);
    assert!(
        heard.iter().any(|m| m == &format!("SelectSession({id:?})")),
        "the click landed: {heard:?}"
    );
}

fn ringed(id: &str) -> [Focused; 1] {
    [Focused {
        id: id.into(),
        ringed: true,
    }]
}

/// Focus `id` from the keyboard, as Tab would leave it.
fn keyboard_focus(window: &mut Window, app: &mut App, id: &str) {
    window.deliver(app, vec![Message::Focus(id.into())]);
    window.key(app, named(iced::keyboard::key::Named::Shift));
    assert_eq!(window.focused(app), ringed(id));
}

/// An entry chosen with Enter in a menu the keyboard opened, when the
/// choice closes the menu, gives the focus back to the menu's opener.
#[test]
fn enter_on_a_menu_entry_gives_the_focus_back_to_its_opener() {
    use iced::keyboard::key::Named;
    let mut app = app();
    project_page(&mut app, "/work/keel");
    let mut window = window(&app);
    keyboard_focus(&mut window, &mut app, "project-more");
    window.key(&mut app, named(Named::Enter));
    assert_eq!(window.focused(&app), ringed("project-tab-Journal"));
    window.key(&mut app, named(Named::Enter));
    assert!(!app.shell.more, "the choice closed the menu");
    assert_eq!(app.screen, Screen::Journal, "and went to History");
    assert_eq!(window.focused(&app), ringed("project-more"));
}

/// The roles menu, opened from the keyboard, takes the focus to its first
/// role; choosing one keeps the menu open and the focus in it; Escape
/// gives the focus back to the tag that opened it.
#[test]
fn the_roles_menu_takes_and_gives_back_the_keyboards_focus() {
    use iced::keyboard::key::Named;
    let mut app = app();
    let root = "/work/orchard";
    let id = session(&mut app, "reviewer-one", root);
    project_page(&mut app, root);
    app.palettes_asked.insert(root.into());
    let palette =
        agentdocker_core::role::defaults(&agentdocker_core::ProjectId::from("orchard"), Utc::now());
    let first = palette[0].role.clone();
    app.palettes.insert(root.into(), Some(palette));
    let _ = app.update(Message::SelectSession(id.clone()));
    let mut window = window(&app);
    keyboard_focus(&mut window, &mut app, "session-roles");
    window.key(&mut app, named(Named::Enter));
    assert_eq!(app.shell.roles_menu.as_deref(), Some(id.as_str()));
    let pick = format!("role-pick-{id}-{first}");
    assert_eq!(window.focused(&app), ringed(&pick));
    window.key(&mut app, named(Named::Escape));
    assert_eq!(app.shell.roles_menu, None);
    assert_eq!(window.focused(&app), ringed("session-roles"));
}

/// The model and effort menus take the keyboard's focus to their first
/// entry when the keyboard opens them, and leave it alone for the pointer.
#[test]
fn the_launch_choice_menus_take_the_focus_only_from_the_keyboard() {
    use super::shell::ChoiceMenu;
    for (which, key) in [(ChoiceMenu::Model, "model"), (ChoiceMenu::Effort, "effort")] {
        let mut app = app();
        let _ = app.update(Message::LaunchChoiceMenu(Some(which)));
        assert_eq!(
            app.shell.menu_opener.as_deref(),
            Some(&*format!("launch-{key}"))
        );
        let _ = app.update(Message::LaunchChoiceMenu(None));
        assert_eq!(app.shell.menu_opener, None, "closed: nothing to give back");
        let _ = app.update(Message::Pressed(format!("launch-{key}")));
        let _ = app.update(Message::LaunchChoiceMenu(Some(which)));
        assert_eq!(app.shell.menu_opener, None, "the pointer opened it");
    }
}

/// Back and Forward pressed with the pointer leave no ring, on the arrow
/// or on the place they land on, and what had the focus where the person
/// went does not follow them back.
#[test]
fn back_and_forward_by_the_pointer_leave_no_ring() {
    let mut app = app();
    let id = session(&mut app, "kestrel", "/work/keel");
    project_page(&mut app, "/work/keel");
    let mut window = window(&app);
    let row = window
        .bounds(&app, &format!("session-{id}"))
        .expect("the row");
    window.tap(&mut app, row.center());
    let board = window
        .bounds(&app, "project-tab-Board")
        .expect("the Board tab");
    window.tap(&mut app, board.center());
    assert_eq!(app.screen, Screen::Board);
    // Something on the board has the keyboard when the person goes back.
    keyboard_focus(&mut window, &mut app, "project-tab-Chat");
    for (arrow, screen) in [
        ("history-back", Screen::Agents),
        ("history-forward", Screen::Board),
        ("history-back", Screen::Agents),
    ] {
        let at = window.bounds(&app, arrow).expect("the arrow");
        window.tap(&mut app, at.center());
        assert_eq!(app.screen, screen, "{arrow}");
        let focused = window.focused(&app);
        assert!(focused.iter().all(|f| !f.ringed), "{arrow}: {focused:?}");
    }
    assert_eq!(app.shell.selected.as_deref(), Some(id.as_str()));
}
