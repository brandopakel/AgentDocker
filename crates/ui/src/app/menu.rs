//! The one menu that is open.
//!
//! Each menu had a field of its own (`more`, `launch_menu`,
//! `project_menu`, `editor_menu`, `roles_menu`, `launch_choice_menu`,
//! `tags_menu`, `card_menu`), and each thing that closes menus named the
//! ones it remembered: going to another screen closed three of them, the
//! palette another three, Back and Forward six, Escape two and then two
//! more on the board. So two menus could be open at once, a menu could
//! be left open under the palette or the launch dialog, and one left
//! open behind a screen was found open on coming back to it.
//!
//! Here there is one value. Opening a menu puts it in the place of
//! whatever was open, so there is never a second; and everything that
//! closes menus calls the one [`App::close_menu`], which cannot forget
//! one.

use std::path::{Path, PathBuf};

use super::App;
use super::shell::{CardMenu, ChoiceMenu, EditorMenu, State};

/// A menu, and what it is a menu of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Menu {
    /// The More tab's destinations.
    More,
    /// The tools **Launch agent** can start.
    LaunchTools,
    /// A project row's menu, by the project's folder.
    Project(PathBuf),
    /// **Open in…**, for the project or for the selected session.
    Editor(EditorMenu),
    /// An agent's roles, by its id.
    Roles(String),
    /// **Model** or **Effort**, in the launch dialog.
    LaunchChoice(ChoiceMenu),
    /// The board's tag filter.
    Tags,
    /// The open card's plan, files or tools.
    Card(CardMenu),
}

impl Menu {
    /// Whether it floats over the page, anchored to the control that
    /// opened it, and so takes and gives back the keyboard's focus. The
    /// board's two are drawn in the page.
    pub fn floats(&self) -> bool {
        !matches!(self, Menu::Tags | Menu::Card(_))
    }
}

impl State {
    /// The open card's menu, when that is the menu that is open.
    pub fn card_menu(&self) -> Option<CardMenu> {
        match &self.menu {
            Some(Menu::Card(menu)) => Some(*menu),
            _ => None,
        }
    }

    /// The agent whose roles menu is open.
    pub fn roles_menu(&self) -> Option<&str> {
        match &self.menu {
            Some(Menu::Roles(agent)) => Some(agent),
            _ => None,
        }
    }

    /// The project whose row menu is open.
    pub fn project_menu(&self) -> Option<&Path> {
        match &self.menu {
            Some(Menu::Project(root)) => Some(root),
            _ => None,
        }
    }

    /// Whether `menu` is the one that is open.
    pub fn menu_is(&self, menu: &Menu) -> bool {
        self.menu.as_ref() == Some(menu)
    }
}

impl App {
    /// Open `menu` in place of whatever menu was open.
    pub(super) fn open_menu(&mut self, menu: Menu) {
        self.close_menu();
        self.shell.menu = Some(menu);
    }

    /// Open `menu`, or close it when it is the one that is open. Returns
    /// whether it is open now.
    pub(super) fn toggle_menu(&mut self, menu: Menu) -> bool {
        if self.shell.menu_is(&menu) {
            self.close_menu();
            false
        } else {
            self.open_menu(menu);
            true
        }
    }

    /// Close the open menu, whichever it is. The one close: everything
    /// that closes menus comes here, so none is left open behind a
    /// screen, under the palette or under the launch dialog.
    ///
    /// What a menu held of its own goes with it: the name being typed in
    /// a project's menu, and in an agent's roles the palette being edited
    /// and the delete pressed once.
    pub(super) fn close_menu(&mut self) {
        self.shell.menu = None;
        self.shell.project_rename = None;
        self.shell.roles_editing = false;
        self.shell.role_delete = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_boards_menus_are_drawn_in_the_page_and_the_rest_float() {
        for menu in [
            Menu::More,
            Menu::LaunchTools,
            Menu::Project(PathBuf::from("/work/app")),
            Menu::Editor(EditorMenu::Project),
            Menu::Roles("agent-1".into()),
            Menu::LaunchChoice(ChoiceMenu::Model),
        ] {
            assert!(menu.floats(), "{menu:?}");
        }
        assert!(!Menu::Tags.floats());
        assert!(!Menu::Card(CardMenu::Start).floats());
    }
}
