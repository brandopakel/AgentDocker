//! The page's standing alerts, each by what raised it.
//!
//! There was one line, `error: Option<String>`, shared by everything
//! that had something to say at the top of the page. Thirteen things set
//! it and whichever came last was what it said. Starting a launch or a
//! reconnect cleared it, whoever had set it: so the warning that the
//! saved workspace could not be read, and that nothing changed in this
//! window would be saved, went away the moment an agent was launched.
//!
//! Here each alert is kept under its source. A source's line is replaced
//! only by a later line of the same source and cleared only by that
//! source's own operation, or by the person dismissing it. Several can
//! stand at once, and each is drawn with its own Dismiss.

use std::collections::BTreeMap;

/// What raised an alert. The order is the order they are shown in: the
/// one about the workspace first, since it is about everything else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// The saved workspace could not be loaded or saved: what changes in
    /// this window will not be kept.
    Workspace,
    /// Removed projects could not be passed to the hooks.
    Exclusions,
    /// A draft is too long, or there is no room for another.
    Draft,
    /// A folder could not be added as a project.
    Folder,
    /// An agent could not be launched.
    Launch,
    /// A session could not be reconnected.
    Reconnect,
    /// An agent's terminal could not be opened.
    Terminal,
    /// A folder could not be opened in an editor.
    Editor,
}

impl Source {
    /// The word a control's id carries for it.
    pub fn word(self) -> &'static str {
        match self {
            Source::Workspace => "workspace",
            Source::Exclusions => "exclusions",
            Source::Draft => "draft",
            Source::Folder => "folder",
            Source::Launch => "launch",
            Source::Reconnect => "reconnect",
            Source::Terminal => "terminal",
            Source::Editor => "editor",
        }
    }
}

/// The alerts that stand, one per source.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Alerts(BTreeMap<Source, String>);

impl Alerts {
    /// `source` has this to say: in place of what it said before, and of
    /// nothing any other source said.
    pub fn raise(&mut self, source: Source, words: impl Into<String>) {
        self.0.insert(source, words.into());
    }

    /// `source`'s own operation began again or went through, or the
    /// person dismissed its line: it has nothing to say. No other
    /// source's line is touched.
    pub fn clear(&mut self, source: Source) {
        self.0.remove(&source);
    }

    /// What `source` says, if it says anything.
    #[cfg(test)]
    pub fn of(&self, source: Source) -> Option<&str> {
        self.0.get(&source).map(String::as_str)
    }

    /// Every alert that stands, in the order they are shown.
    pub fn shown(&self) -> impl Iterator<Item = (Source, &str)> {
        self.0
            .iter()
            .map(|(source, words)| (*source, words.as_str()))
    }

    /// Whether nothing stands.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source's line is its own: another source neither replaces it
    /// nor clears it, and they are shown together, the workspace's first.
    #[test]
    fn an_alert_is_replaced_and_cleared_only_by_its_own_source() {
        let mut alerts = Alerts::default();
        assert!(alerts.is_empty());
        alerts.raise(Source::Launch, "codex is not installed");
        alerts.raise(Source::Workspace, "changes will not be saved");
        assert_eq!(
            alerts.shown().collect::<Vec<_>>(),
            [
                (Source::Workspace, "changes will not be saved"),
                (Source::Launch, "codex is not installed"),
            ],
            "both stand, the workspace's first"
        );
        // Its own source says something else: its line is replaced.
        alerts.raise(Source::Launch, "the daemon refused");
        assert_eq!(alerts.of(Source::Launch), Some("the daemon refused"));
        assert_eq!(
            alerts.of(Source::Workspace),
            Some("changes will not be saved")
        );
        // A launch beginning again clears the launch's, and only that.
        alerts.clear(Source::Launch);
        assert_eq!(alerts.of(Source::Launch), None);
        assert_eq!(
            alerts.of(Source::Workspace),
            Some("changes will not be saved"),
            "the workspace's warning stands"
        );
        // Clearing what says nothing changes nothing.
        alerts.clear(Source::Reconnect);
        assert_eq!(alerts.shown().count(), 1);
    }

    #[test]
    fn every_source_has_a_word_of_its_own() {
        let words: std::collections::BTreeSet<_> = [
            Source::Workspace,
            Source::Exclusions,
            Source::Draft,
            Source::Folder,
            Source::Launch,
            Source::Reconnect,
            Source::Terminal,
            Source::Editor,
        ]
        .map(Source::word)
        .into_iter()
        .collect();
        assert_eq!(words.len(), 8);
    }
}
