//! The command palette: ⌘K (Control+K elsewhere) from anywhere, or the
//! rail's **Jump to…**, opens one search over the places, the projects
//! and the actions the window already offers. It adds nothing new to do:
//! every row sends a message some other control already sends, and the
//! palette closes. **Launch agent…** opens a nested page of the installed
//! tools. Escape clears the search, then backs out of a nested page, then
//! closes; Backspace on an empty search backs out too.
//!
//! While it is open the search keeps the keyboard: Up, Down and Tab move
//! the highlighted row, Enter runs it. The rows are still ordinary
//! accessible controls, pressed by the pointer or by assistive technology.
//! Nothing animates: the palette is a layer over a flat scrim, placed high
//! in the window so the field stays put while the list changes length.
use super::icons::{Icon, icon};
use super::style::{Colors, RADIUS_MD, alpha, weight};
use super::view::{eyebrow, kbd, monogram};
use super::*;
use crate::controls::{Kind, custom, custom_sized};
use iced::advanced::{
    Clipboard, Layout, Shell, Widget, layout, renderer,
    widget::{Operation, Tree, tree},
};
use iced::widget::{Space, column, container, row, scrollable, text};
use iced::{Center, Element, Event, Fill, Length, Rectangle, Size, Task, keyboard, mouse};

/// The search field's id, for focus and for the workflow driver.
const SEARCH: &str = "palette-search";
/// The list's scroll, so a highlighted row can be scrolled into view.
const RESULTS: &str = "palette-results";
/// The modifier the shortcuts are written with on this platform.
const MODIFIER: &str = if cfg!(target_os = "macos") {
    "⌘"
} else {
    "Ctrl"
};
/// What the rail's trigger says to assistive technology.
const TRIGGER_LABEL: &str = if cfg!(target_os = "macos") {
    "Jump to… (⌘K)"
} else {
    "Jump to… (Ctrl+K)"
};
/// The panel's width when the window has room for it.
const WIDTH: f32 = 560.0;
/// The list's tallest: about nine rows, then it scrolls.
const LIST_HEIGHT: f32 = 352.0;

/// A page of the palette above the first one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Page {
    /// The installed tools, one of which to launch in the selected project.
    Launch,
}

/// The palette's state: window-local and never saved.
#[derive(Debug, Default)]
pub(super) struct Palette {
    pub open: bool,
    pub query: String,
    /// Pages opened above the first, innermost last.
    pub pages: Vec<Page>,
    /// The highlighted row: an index into the rows the search leaves.
    pub active: usize,
}

impl Palette {
    /// The page on view; `None` is the first one.
    pub fn page(&self) -> Option<Page> {
        self.pages.last().copied()
    }
    /// Open on the first page with an empty search.
    fn show(&mut self) {
        *self = Self {
            open: true,
            ..Self::default()
        }
    }
    fn close(&mut self) {
        *self = Self::default();
    }
    fn search(&mut self, query: String) {
        self.query = query.chars().take(200).collect();
        self.active = 0;
    }
    fn open_page(&mut self, page: Page) {
        self.pages.push(page);
        self.query.clear();
        self.active = 0;
    }
    /// Escape: clear the search; with nothing typed, back out of a nested
    /// page; on the first page, close. Answers whether it is still open.
    fn escape(&mut self) -> bool {
        if !self.query.is_empty() {
            self.query.clear();
            self.active = 0;
        } else if self.pages.pop().is_some() {
            self.active = 0;
        } else {
            self.close();
        }
        self.open
    }
    /// Back out of a nested page (the back arrow, or Backspace on an
    /// empty search). The first page has nothing behind it.
    fn back(&mut self) -> bool {
        if self.pages.pop().is_none() {
            return false;
        }
        self.query.clear();
        self.active = 0;
        true
    }
    /// Move the highlight by `delta` among `count` rows, round the ends.
    fn step(&mut self, delta: i32, count: usize) {
        if count == 0 {
            self.active = 0;
            return;
        }
        let at = self.active.min(count - 1) as i64;
        self.active = (at + i64::from(delta)).rem_euclid(count as i64) as usize;
    }
}

/// The groups rows are listed under, in the order they appear unsearched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Group {
    GoTo,
    Projects,
    Actions,
    Tools,
}
impl Group {
    fn heading(self) -> &'static str {
        match self {
            Group::GoTo => "Go to",
            Group::Projects => "Projects",
            Group::Actions => "Actions",
            Group::Tools => "Tools",
        }
    }
}

/// What a row shows before its label.
#[derive(Clone, Debug)]
pub(super) enum Mark {
    Glyph(Icon),
    /// A project's own tile.
    Monogram {
        name: String,
        seed: String,
    },
}

/// What a row shows at its right edge.
#[derive(Clone, Debug)]
pub(super) enum Hint {
    None,
    /// The shortcut that does the same thing from anywhere.
    Keys(Vec<&'static str>),
    /// The row opens a page of its own.
    Opens,
}

/// What running a row does.
#[derive(Clone, Debug)]
pub(super) enum Run {
    /// Send this message, the one another control already sends, and
    /// move the focus to the field it opens, if any.
    Send(Box<Message>, Option<&'static str>),
    Open(Page),
}
impl Run {
    fn send(message: Message, field: Option<&'static str>) -> Self {
        Run::Send(Box::new(message), field)
    }
}

#[derive(Clone, Debug)]
pub(super) struct Item {
    /// `palette-item-<kind>-<key>`: the row's control id.
    pub id: String,
    pub group: Group,
    pub label: String,
    /// A short quiet line after the label; searched by word only.
    pub detail: String,
    pub mark: Mark,
    pub hint: Hint,
    pub run: Run,
}

/// The lower-case words of `value`: runs of letters and digits.
fn words(value: &str) -> Vec<&str> {
    value
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect()
}

/// How well a row matches a lower-case, trimmed query; lower is better
/// and `None` is no match. A label that starts with the query is best;
/// then one in which every term of the query starts a word; then a label
/// that holds the query anywhere; last, a row in which every term starts
/// a word of the label or its description.
pub(super) fn rank(query: &str, label: &str, detail: &str) -> Option<u8> {
    let label = label.to_lowercase();
    if label.starts_with(query) {
        return Some(0);
    }
    let terms = words(query);
    let starts = |words: &[&str]| {
        !terms.is_empty() && terms.iter().all(|t| words.iter().any(|w| w.starts_with(t)))
    };
    let mut label_words = words(&label);
    if starts(&label_words) {
        return Some(1);
    }
    if label.contains(query) {
        return Some(2);
    }
    let detail = detail.to_lowercase();
    label_words.extend(words(&detail));
    starts(&label_words).then_some(3)
}

/// The rows a search leaves, best first. Rows stay with their group and
/// groups are ordered by their best row; within a group and a rank, and
/// with nothing typed, the order they were given in stands.
pub(super) fn filter(items: Vec<Item>, query: &str) -> Vec<Item> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return items;
    }
    let mut ranked: Vec<(u8, usize, Item)> = items
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| {
            rank(&query, &item.label, &item.detail).map(|rank| (rank, index, item))
        })
        .collect();
    let mut best: BTreeMap<Group, (u8, usize)> = BTreeMap::new();
    for (rank, index, item) in &ranked {
        let entry = best.entry(item.group).or_insert((*rank, *index));
        entry.0 = entry.0.min(*rank);
    }
    ranked.sort_by_key(|(rank, index, item)| {
        let (group_rank, group_first) = best[&item.group];
        (group_rank, group_first, *rank, *index)
    });
    ranked.into_iter().map(|(_, _, item)| item).collect()
}

/// A path as the person would write it: their home folder as `~`.
fn short_path(path: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from)
        && !home.as_os_str().is_empty()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".to_owned()
        } else {
            format!("~/{}", rest.display())
        };
    }
    path.display().to_string()
}

/// Keys pressed together, as keycaps side by side.
fn combo<'a>(keys: &[&'static str], c: Colors) -> Element<'a, Message> {
    row(keys.iter().map(|key| kbd(*key, c)))
        .spacing(3)
        .align_y(Center)
        .into()
}

/// The container around a row, scrolled to when the highlight reaches it.
fn anchor(id: &str) -> String {
    format!("{id}-row")
}

/// A hairline inside the panel, the same tone as its edge.
fn hairline<'a>(c: Colors) -> Element<'a, Message> {
    let tone = if c.dark { c.line_strong } else { c.line };
    container(Space::new().width(Fill).height(1))
        .style(move |_| container::Style {
            background: Some(tone.into()),
            ..Default::default()
        })
        .into()
}

/// What the keyboard does before any control sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Keys {
    /// Closed: only the shortcut that opens it, which a terminal keeps
    /// where Control+K means something to the shell.
    Closed {
        shortcut: bool,
    },
    Open {
        query_empty: bool,
        nested: bool,
    },
}

/// What becomes of one key press.
#[derive(Debug)]
pub(super) enum Handling {
    /// Not the palette's: the focused control has it.
    Pass,
    /// The palette's, but a held key repeating it does nothing more.
    Swallow,
    Send(Command),
}

/// The palette's own keys, as the messages they send. A small value of its
/// own rather than a `Message`, which is large on some targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Command {
    Palette,
    Move(i32),
    Submit,
    Escape,
    Back,
}

impl From<Command> for Message {
    fn from(command: Command) -> Self {
        match command {
            Command::Palette => Message::CommandPalette,
            Command::Move(step) => Message::CommandMove(step),
            Command::Submit => Message::CommandSubmit,
            Command::Escape => Message::CommandEscape,
            Command::Back => Message::CommandBack,
        }
    }
}

/// The palette's reading of a key press.
pub(super) fn handle(
    keys: Keys,
    key: &keyboard::Key,
    physical: keyboard::key::Physical,
    modifiers: keyboard::Modifiers,
    repeat: bool,
) -> Handling {
    use keyboard::{Key, key::Named};
    let once = |message: Command| {
        if repeat {
            Handling::Swallow
        } else {
            Handling::Send(message)
        }
    };
    let shortcut = modifiers.command()
        && !modifiers.shift()
        && !modifiers.alt()
        && key.to_latin(physical) == Some('k');
    match keys {
        Keys::Closed { shortcut: true } if shortcut => once(Command::Palette),
        Keys::Closed { .. } => Handling::Pass,
        Keys::Open { .. } if shortcut => once(Command::Palette),
        Keys::Open {
            query_empty,
            nested,
        } => match key {
            Key::Named(Named::Escape) => once(Command::Escape),
            Key::Named(Named::Enter) => once(Command::Submit),
            Key::Named(Named::ArrowDown) => Handling::Send(Command::Move(1)),
            Key::Named(Named::ArrowUp) => Handling::Send(Command::Move(-1)),
            Key::Named(Named::Tab) => {
                Handling::Send(Command::Move(if modifiers.shift() { -1 } else { 1 }))
            }
            Key::Named(Named::Backspace) if query_empty && nested => once(Command::Back),
            _ => Handling::Pass,
        },
    }
}

/// The window wrapped so the palette's keys reach it ahead of every
/// control in it: ⌘K from a focused field types nothing, and Escape in
/// the search does not just unfocus it. A composition in an input method
/// keeps its own keys.
struct Shortcuts<'a> {
    content: Element<'a, Message>,
    keys: Keys,
}

/// Whether an input method is composing; its Enter and Escape are its own.
#[derive(Default)]
struct Composing(bool);

impl Widget<Message, iced::Theme, iced::Renderer> for Shortcuts<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Composing>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(Composing::default())
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }
    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        use iced::advanced::input_method;
        let composing = tree.state.downcast_mut::<Composing>();
        match event {
            Event::InputMethod(input_method::Event::Preedit(text, _)) => {
                composing.0 = !text.is_empty();
            }
            Event::InputMethod(input_method::Event::Commit(_) | input_method::Event::Closed) => {
                composing.0 = false;
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                repeat,
                ..
            }) if !composing.0 => {
                match handle(self.keys, key, *physical_key, *modifiers, *repeat) {
                    Handling::Pass => {}
                    Handling::Swallow => {
                        shell.capture_event();
                        return;
                    }
                    Handling::Send(command) => {
                        shell.publish(command.into());
                        shell.capture_event();
                        return;
                    }
                }
            }
            _ => {}
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: iced::Vector,
    ) -> Option<iced::advanced::overlay::Element<'a, Message, iced::Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

/// The search field: no frame of its own, the panel's top band is its
/// field. Enter is the palette's (it runs the highlighted row), so the
/// accessibility node carries that as the field's action.
fn search_field<'a>(placeholder: &str, value: &str, c: Colors) -> Element<'a, Message> {
    let field = iced::widget::text_input(placeholder, value)
        .id(SEARCH)
        .size(14)
        .line_height(iced::Pixels(20.0))
        .padding([6, 0])
        .on_input(Message::CommandQuery)
        .style(move |_, _| iced::widget::text_input::Style {
            background: iced::Color::TRANSPARENT.into(),
            border: iced::Border::default(),
            icon: c.muted,
            placeholder: c.faint,
            value: c.text,
            selection: alpha(c.accent, 0.35),
        });
    let mut semantic = crate::accessibility::Semantic::input(
        SEARCH.to_owned(),
        placeholder.to_owned(),
        value.to_owned(),
        std::sync::Arc::new(Message::CommandQuery),
    );
    semantic.action = Some(Message::CommandSubmit);
    crate::controls::Control {
        content: field.into(),
        semantic,
        button: false,
    }
    .into()
}

/// One row: its mark, its label and quiet description on one line, and
/// at its right edge the shortcut that does the same, or a chevron for a
/// row that opens a page. The highlighted row is the selected one.
fn item_view<'a>(item: &Item, active: bool, c: Colors) -> Element<'a, Message> {
    let ink = if active { c.accent_ink } else { c.muted };
    let mark: Element<'a, Message> = match &item.mark {
        Mark::Glyph(glyph) => container(icon(*glyph, ink, 16.0)).center(20).into(),
        Mark::Monogram { name, seed } => monogram(name, seed, 20.0, c),
    };
    let mut words = row![
        text(item.label.clone())
            .size(13.5)
            .line_height(iced::Pixels(20.0))
            .font(weight(iced::font::Weight::Medium))
            .wrapping(iced::widget::text::Wrapping::None)
    ]
    .spacing(8)
    .align_y(Center);
    if !item.detail.is_empty() {
        words = words.push(
            text(item.detail.clone())
                .size(12)
                .color(c.muted)
                .wrapping(iced::widget::text::Wrapping::None),
        );
    }
    let mut line = row![mark, container(words).width(Fill).clip(true)]
        .spacing(10)
        .align_y(Center);
    match &item.hint {
        Hint::None => {}
        Hint::Keys(keys) => line = line.push(combo(keys, c)),
        Hint::Opens => line = line.push(icon(Icon::ChevronRight, ink, 14.0)),
    }
    let spoken = if item.detail.is_empty() {
        item.label.clone()
    } else {
        format!("{}, {}", item.label, item.detail)
    };
    container(custom(
        item.id.clone(),
        spoken,
        line,
        Some(Message::CommandRun(item.id.clone())),
        active,
        Kind::Quiet,
        [8, 10],
    ))
    .id(anchor(&item.id))
    .into()
}

impl App {
    /// The rail's way in, under the brand: a search-looking field with
    /// the shortcut on its right, in the rail's own ink.
    pub(super) fn palette_trigger(&self, r: Colors) -> Element<'_, Message> {
        let field = container(
            row![
                icon(Icon::Search, r.muted, 14.0),
                text("Jump to…")
                    .size(13)
                    .color(r.muted)
                    .width(Fill)
                    .wrapping(iced::widget::text::Wrapping::None),
                combo(&[MODIFIER, "K"], r),
            ]
            .spacing(8)
            .align_y(Center),
        )
        .padding([7, 10])
        .width(Fill)
        .style(move |_| container::Style {
            background: Some(alpha(r.text, 0.04).into()),
            border: iced::Border {
                color: r.line,
                width: 1.0,
                radius: RADIUS_MD.into(),
            },
            ..Default::default()
        });
        custom(
            "command-palette",
            TRIGGER_LABEL,
            field,
            Some(Message::CommandPalette),
            false,
            Kind::Nav,
            [0, 0],
        )
    }

    /// The window with the palette's keys ahead of everything in it.
    pub(super) fn palette_keys<'a>(&self, window: Element<'a, Message>) -> Element<'a, Message> {
        let palette = &self.shell.palette;
        let keys = if palette.open {
            Keys::Open {
                query_empty: palette.query.is_empty(),
                nested: !palette.pages.is_empty(),
            }
        } else {
            // On macOS ⌘K is never the shell's; elsewhere Control+K in a
            // terminal is, and the terminal gets it first.
            Keys::Closed {
                shortcut: cfg!(target_os = "macos") || self.screen != Screen::Terminal,
            }
        };
        Element::new(Shortcuts {
            content: window,
            keys,
        })
    }

    /// Every row of the page on view, unsearched.
    pub(super) fn palette_items(&self) -> Vec<Item> {
        match self.shell.palette.page() {
            Some(Page::Launch) => self.palette_tools(),
            None => self.palette_places(),
        }
    }

    /// The rows the search leaves on the page on view.
    fn palette_rows(&self) -> Vec<Item> {
        filter(self.palette_items(), &self.shell.palette.query)
    }

    /// The first page: places, every project, and the actions there are.
    fn palette_places(&self) -> Vec<Item> {
        let shortcut = |digit: &'static str| Hint::Keys(vec![MODIFIER, digit]);
        let go = |key: &str, label: &str, detail: &str, glyph: Icon, message: Message, hint| Item {
            id: format!("palette-item-go-{key}"),
            group: Group::GoTo,
            label: label.to_owned(),
            detail: detail.to_owned(),
            mark: Mark::Glyph(glyph),
            hint,
            run: Run::send(message, None),
        };
        let conversations = self.has_conversations();
        let mut items = vec![
            go(
                "projects",
                "All projects",
                "Every project and its agents",
                Icon::Projects,
                Message::AllProjects,
                Hint::None,
            ),
            go(
                "inbox",
                if conversations { "Messages" } else { "Inbox" },
                if conversations {
                    "Conversations and channels"
                } else {
                    "Questions and messages for you"
                },
                Icon::Inbox,
                Message::Navigate(Screen::Questions),
                shortcut("2"),
            ),
            go(
                "tools",
                "Tools",
                "Installed tools and their connections",
                Icon::Connections,
                Message::Navigate(Screen::Runtimes),
                shortcut("3"),
            ),
            go(
                "settings",
                "Settings",
                "Appearance, terminal and installation",
                Icon::Settings,
                Message::Navigate(Screen::Settings),
                shortcut("4"),
            ),
        ];
        for entry in &self.shell.catalog.projects {
            let path = &entry.project.root;
            let name = entry.name();
            // Where it is, without saying its name twice: the folder it
            // sits in, or the whole path when it goes by another name.
            let detail = match path.parent() {
                Some(parent) if path.file_name().is_some_and(|f| *f == *name) => short_path(parent),
                _ => short_path(path),
            };
            items.push(Item {
                id: format!("palette-item-project-{}", path.display()),
                group: Group::Projects,
                label: name.clone(),
                detail,
                mark: Mark::Monogram {
                    name,
                    seed: entry.project.id().to_string(),
                },
                hint: Hint::None,
                run: Run::send(Message::SelectProject(path.clone()), None),
            });
        }
        let action = |key: &str, label: &str, detail: String, glyph: Icon, hint, run| Item {
            id: format!("palette-item-action-{key}"),
            group: Group::Actions,
            label: label.to_owned(),
            detail,
            mark: Mark::Glyph(glyph),
            hint,
            run,
        };
        // The project's actions are offered when its own controls are
        // enabled, and only then.
        if let Some(entry) = self.shell.catalog.selected() {
            let name = entry.name();
            let available = self.shell.project_available != Some(false);
            if self.connected.is_ok() && available {
                items.push(action(
                    "launch",
                    "Launch agent…",
                    format!("In {name}"),
                    Icon::Sessions,
                    Hint::Opens,
                    Run::Open(Page::Launch),
                ));
            }
            if available && !self.shell.terminal_opening {
                items.push(action(
                    "terminal",
                    "Open project terminal",
                    format!("A shell in {name}"),
                    Icon::Terminal,
                    Hint::None,
                    Run::send(Message::OpenProjectTerminal, None),
                ));
            }
            // Pause… is the header's, so it is offered with the header on
            // view: the reason is typed under it.
            let root = entry.project.root.display().to_string();
            let paused = self.pauses.iter().any(|p| p.project == entry.project.id());
            let busy = self
                .pause_states
                .get(&root)
                .is_some_and(|control| control.pending.is_some() || control.draft.is_some());
            let on_view = !matches!(
                self.screen,
                Screen::Questions | Screen::Runtimes | Screen::Settings | Screen::Desktop
            );
            if on_view && self.connected.is_ok() && !paused && !busy {
                items.push(action(
                    "pause",
                    "Pause…",
                    format!("Ask the agents in {name} to hold"),
                    Icon::Pause,
                    Hint::None,
                    Run::send(Message::PauseStart(root), Some("pause-reason")),
                ));
            }
        }
        items.push(action(
            "add-project",
            "Add project…",
            "A folder you already have".to_owned(),
            Icon::Add,
            Hint::None,
            if self.shell.adding {
                Run::send(Message::Focus("project-path".into()), None)
            } else {
                Run::send(Message::ShowAdd, Some("project-path"))
            },
        ));
        let dark = self.shell.catalog.dark;
        items.push(action(
            "appearance",
            if dark {
                "Use light appearance"
            } else {
                "Use dark appearance"
            },
            String::new(),
            Icon::Contrast,
            Hint::None,
            Run::send(Message::Dark(!dark), None),
        ));
        items
    }

    /// Launch agent…'s page: the installed tools, as its menu lists them.
    fn palette_tools(&self) -> Vec<Item> {
        self.runtimes
            .iter()
            .filter_map(|runtime| {
                let cli = runtime.cli.as_ref()?;
                Some(Item {
                    id: format!("palette-item-tool-{}", runtime.name),
                    group: Group::Tools,
                    label: runtime.label.clone(),
                    detail: short_path(cli),
                    mark: Mark::Glyph(Icon::Terminal),
                    hint: Hint::None,
                    run: Run::send(Message::LaunchWith(runtime.name.clone()), None),
                })
            })
            .collect()
    }

    /// Give the search the keyboard.
    fn focus_search(&self) -> Task<Message> {
        iced::widget::operation::focus(SEARCH).chain(crate::accessibility::collect())
    }

    /// Scroll the highlighted row into view.
    fn reveal_active(&self) -> Task<Message> {
        let rows = self.palette_rows();
        let active = self.shell.palette.active.min(rows.len().saturating_sub(1));
        match rows.get(active) {
            Some(item) => crate::controls::reveal(anchor(&item.id)),
            None => Task::none(),
        }
    }

    /// Run one row: open its page, or close and send its message.
    fn palette_run(&mut self, id: &str) -> Task<Message> {
        if !self.shell.palette.open {
            return Task::none();
        }
        let Some(item) = self.palette_items().into_iter().find(|item| item.id == id) else {
            return Task::none();
        };
        match item.run {
            Run::Open(page) => {
                self.shell.palette.open_page(page);
                self.focus_search()
            }
            Run::Send(message, field) => {
                self.shell.palette.close();
                let task = self.update(*message);
                match field {
                    // The field appears with the next view, before the
                    // focus operation runs.
                    Some(field) => Task::batch([
                        task,
                        iced::widget::operation::focus(field)
                            .chain(crate::controls::reveal_focus())
                            .chain(crate::accessibility::collect()),
                    ]),
                    None => task,
                }
            }
        }
    }

    /// The palette's messages; `update` hands them here.
    pub(super) fn command_palette(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::CommandPalette => {
                if self.shell.palette.open {
                    self.shell.palette.close();
                } else {
                    self.shell.palette.show();
                    // A menu left open would float over the palette.
                    self.shell.launch_menu = false;
                    self.shell.project_menu = None;
                    self.shell.more = false;
                    return self.focus_search();
                }
            }
            Message::CommandQuery(query) if self.shell.palette.open => {
                self.shell.palette.search(query);
                return self.reveal_active();
            }
            Message::CommandMove(delta) if self.shell.palette.open => {
                let count = self.palette_rows().len();
                self.shell.palette.step(delta, count);
                return self.reveal_active();
            }
            Message::CommandSubmit if self.shell.palette.open => {
                let rows = self.palette_rows();
                let active = self.shell.palette.active.min(rows.len().saturating_sub(1));
                if let Some(id) = rows.get(active).map(|item| item.id.clone()) {
                    return self.palette_run(&id);
                }
            }
            Message::CommandRun(id) => return self.palette_run(&id),
            Message::CommandEscape => {
                let open = self.shell.palette.escape();
                return if open {
                    self.focus_search()
                } else {
                    Task::none()
                };
            }
            Message::CommandBack => {
                let backed = self.shell.palette.back();
                return if backed {
                    self.focus_search()
                } else {
                    Task::none()
                };
            }
            _ => {}
        }
        Task::none()
    }

    /// The palette over the window: a flat scrim that closes it when
    /// pressed, and the panel high in the window, which takes its own
    /// presses.
    pub(super) fn palette_view(&self, c: Colors) -> Element<'_, Message> {
        let palette = &self.shell.palette;
        let rows = self.palette_rows();
        let active = palette.active.min(rows.len().saturating_sub(1));
        let nested = palette.page();
        let lead: Element<'_, Message> = match nested {
            Some(_) => custom_sized(
                "palette-back",
                "Back",
                container(icon(Icon::ChevronLeft, c.muted, 14.0)).center(18),
                Some(Message::CommandBack),
                false,
                Kind::Ghost,
                [3, 3],
                Length::Shrink,
            ),
            None => container(icon(Icon::Search, c.muted, 16.0))
                .center_x(24)
                .into(),
        };
        let placeholder = match nested {
            Some(Page::Launch) => "Launch an agent with…",
            None => "Jump to…",
        };
        let search = container(
            row![lead, search_field(placeholder, &palette.query, c)]
                .spacing(10)
                .align_y(Center),
        )
        .padding([0, 14])
        .center_y(48);

        let mut list = column![].spacing(1).width(Fill);
        let mut group = None;
        for (index, item) in rows.iter().enumerate() {
            if group != Some(item.group) {
                list = list.push(container(eyebrow(item.group.heading(), c)).padding(
                    iced::Padding {
                        top: if group.is_some() { 12.0 } else { 6.0 },
                        right: 10.0,
                        bottom: 4.0,
                        left: 10.0,
                    },
                ));
                group = Some(item.group);
            }
            list = list.push(item_view(item, index == active, c));
        }
        if rows.is_empty() {
            let words = match nested {
                Some(Page::Launch) if palette.query.trim().is_empty() => {
                    "No tool to launch is installed.".to_owned()
                }
                _ => format!("Nothing matches “{}”", palette.query.trim()),
            };
            list = list.push(
                container(text(words).size(13).color(c.muted))
                    .padding([24, 10])
                    .center_x(Fill),
            );
        }
        // The list gives way first when the window is short.
        let units = self.shell.height / self.scale_factor();
        let top = (units * 0.14).clamp(16.0, 120.0);
        let room = (units - top - 48.0 - 36.0 - 2.0 - 16.0 - 12.0).max(108.0);
        let results = container(
            scrollable(container(list).padding(6))
                .id(RESULTS)
                .height(iced::Shrink)
                .direction(iced::widget::scrollable::Direction::Vertical(
                    iced::widget::scrollable::Scrollbar::new()
                        .width(4)
                        .scroller_width(4)
                        .margin(2),
                )),
        )
        .max_height(LIST_HEIGHT.min(room));

        let tip = |keys: &[&'static str], words: &'static str| -> Element<'static, Message> {
            row![combo(keys, c), text(words).size(12).color(c.muted)]
                .spacing(6)
                .align_y(Center)
                .into()
        };
        let footer = container(
            row![
                tip(&["↵"], "Select"),
                tip(&["↑", "↓"], "Navigate"),
                tip(&["esc"], if nested.is_some() { "Back" } else { "Close" }),
            ]
            .spacing(16)
            .align_y(Center),
        )
        .padding([8, 14]);

        let panel = container(column![search, hairline(c), results, hairline(c), footer])
            .width(Fill)
            .max_width(WIDTH)
            .style(move |_| c.dialog_style());
        let scrim = iced::widget::mouse_area(
            container(iced::widget::opaque(panel))
                .center_x(Fill)
                .align_top(Fill)
                .padding(iced::Padding {
                    top,
                    right: 16.0,
                    bottom: 16.0,
                    left: 16.0,
                })
                .style(move |_| container::Style {
                    background: Some(
                        alpha(iced::Color::BLACK, if c.dark { 0.6 } else { 0.45 }).into(),
                    ),
                    ..Default::default()
                }),
        )
        .on_press(Message::CommandPalette);
        iced::widget::opaque(scrim)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(group: Group, label: &str, detail: &str) -> Item {
        Item {
            id: format!("palette-item-test-{label}"),
            group,
            label: label.into(),
            detail: detail.into(),
            mark: Mark::Glyph(Icon::Projects),
            hint: Hint::None,
            run: Run::send(Message::Tick, None),
        }
    }
    fn labels(items: &[Item]) -> Vec<&str> {
        items.iter().map(|item| item.label.as_str()).collect()
    }
    fn app() -> App {
        let (tx, _) = queue::channel();
        let (_, rx) = std::sync::mpsc::sync_channel(MESSAGE_CAPACITY);
        App::bare(tx, rx)
    }
    fn press(keys: Keys, key: keyboard::Key, modifiers: keyboard::Modifiers) -> Handling {
        handle(
            keys,
            &key,
            keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified),
            modifiers,
            false,
        )
    }

    /// Nothing typed lists every row as given; a label that starts with
    /// the search comes before one with a word that does, which comes
    /// before one that only holds it; case never matters.
    #[test]
    fn a_label_that_starts_with_the_search_ranks_before_a_word_and_a_substring() {
        let rows = || {
            vec![
                item(Group::Actions, "Open project terminal", ""),
                item(Group::Actions, "Determinism check", ""),
                item(Group::Actions, "Terminal", ""),
                item(Group::Actions, "Settings", ""),
            ]
        };
        assert_eq!(
            labels(&filter(rows(), "  ")),
            [
                "Open project terminal",
                "Determinism check",
                "Terminal",
                "Settings"
            ]
        );
        assert_eq!(
            labels(&filter(rows(), "TERM")),
            ["Terminal", "Open project terminal", "Determinism check"]
        );
        // Every term must start a word, in any order.
        assert_eq!(
            labels(&filter(rows(), "term open")),
            ["Open project terminal"]
        );
        assert_eq!(rank("set", "Settings", ""), Some(0));
        assert_eq!(rank("xyz", "Settings", ""), None);
    }

    /// The description counts last and only by whole-word prefixes, so a
    /// project is found by its folder but a path does not match every
    /// letter; a search of punctuation matches only literally.
    #[test]
    fn a_description_matches_by_word_after_every_label_match() {
        assert_eq!(rank("code", "agentdocker", "~/code/agentdocker"), Some(3));
        assert_eq!(rank("ode", "agentdocker", "~/code/agentdocker"), None);
        assert_eq!(rank("…", "Settings", "a … b"), None);
        assert_eq!(rank("…", "Add project…", ""), Some(2));
    }

    /// Rows stay under their group; groups follow their best row, so the
    /// first row is always the best match, and keep their order otherwise.
    #[test]
    fn groups_follow_their_best_row_and_keep_their_order_otherwise() {
        let rows = vec![
            item(Group::GoTo, "Settings", ""),
            item(Group::Projects, "notes", "~/src/terms"),
            item(Group::Projects, "determinism", "~/src/determinism"),
            item(Group::Actions, "Open project terminal", ""),
        ];
        let found = filter(rows, "term");
        assert_eq!(
            labels(&found),
            ["Open project terminal", "determinism", "notes"]
        );
        assert_eq!(found[0].group, Group::Actions);
    }

    /// Escape clears the search first, then backs out of a nested page,
    /// and only then closes. Backspace backs out of a nested page when the
    /// search is empty and is the field's own otherwise.
    #[test]
    fn escape_clears_then_backs_out_then_closes_and_backspace_backs_out_when_empty() {
        let mut palette = Palette::default();
        palette.show();
        palette.open_page(Page::Launch);
        palette.search("cod".into());
        palette.active = 1;
        assert!(palette.escape());
        assert_eq!(palette.query, "");
        assert_eq!(palette.page(), Some(Page::Launch));
        assert_eq!(palette.active, 0);
        assert!(palette.escape());
        assert_eq!(palette.page(), None);
        assert!(!palette.escape());
        assert!(!palette.open);

        palette.show();
        assert!(!palette.back(), "the first page has nothing behind it");
        assert!(palette.open);
        palette.open_page(Page::Launch);
        assert!(palette.back());
        assert_eq!(palette.page(), None);

        let backspace = keyboard::Key::Named(keyboard::key::Named::Backspace);
        let none = keyboard::Modifiers::empty();
        let nested = |query_empty| Keys::Open {
            query_empty,
            nested: true,
        };
        assert!(matches!(
            press(nested(true), backspace.clone(), none),
            Handling::Send(Command::Back)
        ));
        assert!(matches!(
            press(nested(false), backspace.clone(), none),
            Handling::Pass
        ));
        assert!(matches!(
            press(
                Keys::Open {
                    query_empty: true,
                    nested: false
                },
                backspace,
                none
            ),
            Handling::Pass
        ));
    }

    /// The shortcut opens the palette from anywhere, and closes it; while
    /// it is open its keys are its own; closed, Escape and arrows stay the
    /// window's, and a terminal that owns Control+K keeps it.
    #[test]
    fn the_shortcut_toggles_and_the_open_palette_keeps_its_keys() {
        use keyboard::{Key, key::Named};
        let k = Key::Character("k".into());
        let command = keyboard::Modifiers::COMMAND;
        let none = keyboard::Modifiers::empty();
        let open = Keys::Open {
            query_empty: false,
            nested: false,
        };
        let closed = Keys::Closed { shortcut: true };
        assert!(matches!(
            press(closed, k.clone(), command),
            Handling::Send(Command::Palette)
        ));
        assert!(matches!(
            press(open, k.clone(), command),
            Handling::Send(Command::Palette)
        ));
        assert!(matches!(
            press(Keys::Closed { shortcut: false }, k.clone(), command),
            Handling::Pass
        ));
        assert!(matches!(press(closed, k.clone(), none), Handling::Pass));
        assert!(matches!(press(open, k, none), Handling::Pass), "typing a k");
        assert!(matches!(
            press(closed, Key::Named(Named::Escape), none),
            Handling::Pass
        ));
        assert!(matches!(
            press(open, Key::Named(Named::Escape), none),
            Handling::Send(Command::Escape)
        ));
        assert!(matches!(
            press(open, Key::Named(Named::ArrowDown), none),
            Handling::Send(Command::Move(1))
        ));
        assert!(matches!(
            press(open, Key::Named(Named::Tab), keyboard::Modifiers::SHIFT),
            Handling::Send(Command::Move(-1))
        ));
        assert!(matches!(
            press(open, Key::Named(Named::Enter), none),
            Handling::Send(Command::Submit)
        ));
        assert!(matches!(
            handle(
                open,
                &Key::Named(Named::Enter),
                keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified),
                none,
                true,
            ),
            Handling::Swallow
        ));
    }

    /// The highlight goes round the ends and survives a shorter list.
    #[test]
    fn the_highlight_wraps_round_the_rows() {
        let mut palette = Palette::default();
        palette.step(-1, 3);
        assert_eq!(palette.active, 2);
        palette.step(1, 3);
        assert_eq!(palette.active, 0);
        palette.active = 7;
        palette.step(1, 3);
        assert_eq!(palette.active, 0);
        palette.step(1, 0);
        assert_eq!(palette.active, 0);
    }

    /// A row runs the message its own control sends and the palette
    /// closes; the highlight follows the search; Launch agent… opens the
    /// tools and the one chosen opens the launch form with it.
    #[test]
    fn running_a_row_sends_its_message_and_launch_agent_opens_the_tools() {
        let mut app = app();
        app.connected = Ok(());
        let dir = tempfile::tempdir().unwrap();
        let project = agentdocker_core::ProjectRef::directory(dir.path().join("alpha"));
        app.shell.catalog.remember(project.clone(), true);
        app.shell.catalog.selected = Some(project.root.clone());
        app.runtimes = vec![agentdocker_core::runtime::RuntimeInfo {
            name: "codex".into(),
            vendor: "fixture".into(),
            label: "Codex".into(),
            cli: Some("/fixture/codex".into()),
            version: None,
            apps: vec![],
            extensions: vec![],
            incomplete: vec![],
            config_dir: None,
            mcp: agentdocker_core::runtime::Wiring::Missing,
            hooks: agentdocker_core::runtime::Wiring::Missing,
            hooks_missing: vec![],
            shell: agentdocker_core::runtime::Wiring::Unsupported,
            running: 0,
        }];

        let _ = app.update(Message::CommandPalette);
        assert!(app.shell.palette.open);
        let _ = app.update(Message::CommandQuery("SETT".into()));
        let _ = app.update(Message::CommandSubmit);
        assert_eq!(app.screen, Screen::Settings);
        assert!(!app.shell.palette.open);

        let _ = app.update(Message::CommandPalette);
        let _ = app.update(Message::CommandQuery("launch".into()));
        let _ = app.update(Message::CommandSubmit);
        assert!(app.shell.palette.open, "a page, not an action");
        assert_eq!(app.shell.palette.page(), Some(Page::Launch));
        assert_eq!(app.shell.palette.query, "");
        assert_eq!(
            labels(&app.palette_rows()),
            ["Codex"],
            "only installed tools"
        );
        let _ = app.update(Message::CommandRun("palette-item-tool-codex".into()));
        assert!(!app.shell.palette.open);
        assert!(app.shell.launch);
        assert_eq!(app.shell.launch_runtime.as_deref(), Some("codex"));

        // Escape from the first page with nothing typed closes it; a stale
        // row press after that does nothing.
        let _ = app.update(Message::CommandPalette);
        let _ = app.update(Message::CommandEscape);
        assert!(!app.shell.palette.open);
        let _ = app.update(Message::CommandRun("palette-item-go-tools".into()));
        assert_ne!(app.screen, Screen::Runtimes);
    }
}
