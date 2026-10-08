//! What a session changed on its branch, against the branch in the
//! project's main folder, and the one button that merges it.
//!
//! The section reads as the review does: the branch and its target with
//! the size of the change, the files (each opens its own diff), a short
//! checklist of what a merge needs, and **Merge into <target>** — enabled
//! only when nothing stops it. A refusal or a merge says so under the
//! button, in the daemon's words.
use super::*;
use agentdocker_core::review::{Blocker, Review};
use iced::widget::{Space, column, container, row, scrollable, text};
use iced::{Center, Element, Fill, Font};

/// At most this many lines of one file's diff are drawn.
const FILE_LINES: usize = 300;

/// Each file's lines in a unified diff, by its new path: the hunk headers
/// and the changed and context lines, without git's own headers.
fn file_diffs(patch: &str) -> std::collections::HashMap<String, Vec<&str>> {
    let mut files = std::collections::HashMap::new();
    let mut current: Option<String> = None;
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            current = rest.rsplit_once(" b/").map(|(_, path)| path.to_owned());
            if let Some(path) = &current {
                files.entry(path.clone()).or_insert_with(Vec::new);
            }
            continue;
        }
        if line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("new file mode")
            || line.starts_with("deleted file mode")
            || line.starts_with("similarity index")
            || line.starts_with("rename from")
            || line.starts_with("rename to")
        {
            continue;
        }
        if let Some(path) = &current {
            files
                .entry(path.clone())
                .or_insert_with(Vec::new)
                .push(line);
        }
    }
    files
}

impl App {
    pub(super) fn changes_section(
        &self,
        agent: &AgentRecord,
        c: Colors,
    ) -> Option<Element<'_, Message>> {
        let (id, result) = self.branch_review.as_ref()?;
        if id != agent.id.as_str() {
            return None;
        }
        let review = match result {
            Ok(review) => review,
            Err(reason) => {
                return Some(
                    column![
                        eyebrow("Changes", c),
                        small(format!("Could not be read: {reason}"), c)
                    ]
                    .spacing(6)
                    .into(),
                );
            }
        };
        let blockers = review.blockers();
        // An agent working straight in the main folder has nothing apart to
        // merge, and a checkout with no branch has nothing to name.
        if blockers
            .iter()
            .any(|b| matches!(b, Blocker::SameCheckout | Blocker::NoBranch))
        {
            return None;
        }
        let target = review.target.clone().unwrap_or_default();
        let branch = review.branch.clone().unwrap_or_default();
        let (added, removed) = review.totals();
        let plural = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
        let mut section = column![
            row![
                eyebrow("Changes", c),
                Space::new().width(Fill),
                ghost(
                    "review-refresh",
                    "Refresh",
                    Some(Message::ReviewChanges(id.clone()))
                ),
            ]
            .align_y(Center),
            text(format!(
                "{branch} → {target} · {} · {} · +{added} −{removed}",
                plural(review.commits.len(), "commit"),
                plural(review.files.len(), "file"),
            ))
            .size(13)
            .color(c.text),
        ]
        .spacing(8)
        .width(Fill);
        if review.commits.is_empty() {
            section = section.push(small(
                format!("Nothing on {branch} that {target} does not have."),
                c,
            ));
            return Some(section.into());
        }
        let diffs = file_diffs(&review.patch);
        let mut files = column![].spacing(2).width(Fill);
        for file in &review.files {
            let open = self.shell.diff_file.as_deref() == Some(file.path.as_str());
            let counts = match (file.added, file.removed) {
                (Some(a), Some(r)) => format!("+{a} −{r}"),
                _ => "binary".to_owned(),
            };
            files = files.push(custom(
                format!("changes-file-{}", file.path),
                format!("{} {} {counts}", file.status, file.path),
                row![
                    icon(
                        if open {
                            Icon::ChevronDown
                        } else {
                            Icon::ChevronRight
                        },
                        c.muted,
                        11.0
                    ),
                    text(file.status.clone())
                        .size(12)
                        .font(Font::MONOSPACE)
                        .color(match file.status.as_str() {
                            "A" => c.green,
                            "D" => c.red,
                            _ => c.amber,
                        })
                        .width(14),
                    text(file.path.clone())
                        .size(12)
                        .font(Font::MONOSPACE)
                        .color(c.text)
                        .width(Fill)
                        .wrapping(iced::widget::text::Wrapping::None),
                    text(counts).size(12).font(Font::MONOSPACE).color(c.muted),
                ]
                .spacing(6)
                .align_y(Center),
                Some(Message::ToggleDiffFile(file.path.clone())),
                open,
                Kind::Quiet,
                [3, 6],
            ));
            if open {
                files = files.push(self.diff_lines(diffs.get(&file.path), review, c));
            }
        }
        section = section.push(files);
        section = section.push(self.merge_checklist(review, &blockers, &target, c));
        let head = review.head.clone().unwrap_or_default();
        let merging = self.merging.as_deref() == Some(id.as_str());
        let ready = blockers.is_empty() && !merging && self.connected.is_ok();
        section = section.push(primary(
            "merge-branch",
            if merging {
                "Merging…".to_owned()
            } else {
                format!("Merge into {target}")
            },
            ready.then(|| Message::MergeBranch(id.clone(), head)),
        ));
        if let Some((outcome_for, outcome)) = &self.merge_outcome
            && outcome_for == id
        {
            section = section.push(match outcome {
                Ok(said) => text(said.clone()).size(12.5).color(c.green),
                Err(reason) => text(reason.clone()).size(12.5).color(c.amber),
            });
        }
        Some(section.into())
    }

    /// One file's diff: added lines green, removed red, hunk headers in the
    /// accent, at most [`FILE_LINES`] of them.
    fn diff_lines(
        &self,
        lines: Option<&Vec<&str>>,
        review: &Review,
        c: Colors,
    ) -> Element<'_, Message> {
        let Some(lines) = lines.filter(|l| !l.is_empty()) else {
            return container(small(
                if review.truncated {
                    "This diff is too large to show here; the file list is complete."
                } else {
                    "No text changes to show (a binary file, or a rename)."
                },
                c,
            ))
            .padding([4, 24])
            .into();
        };
        let mut body = column![].width(Fill);
        for line in lines.iter().take(FILE_LINES) {
            let (tone, wash) = match line.chars().next() {
                Some('+') => (
                    c.green,
                    Some(alpha(c.green, if c.dark { 0.12 } else { 0.08 })),
                ),
                Some('-') => (c.red, Some(alpha(c.red, if c.dark { 0.12 } else { 0.08 }))),
                Some('@') => (c.accent, None),
                _ => (c.muted, None),
            };
            body = body.push(
                container(
                    text((*line).to_owned())
                        .size(11.5)
                        .font(Font::MONOSPACE)
                        .color(tone)
                        .wrapping(iced::widget::text::Wrapping::None),
                )
                .padding([1, 8])
                .width(Fill)
                .style(move |_| container::Style {
                    background: wash.map(Into::into),
                    ..Default::default()
                }),
            );
        }
        if lines.len() > FILE_LINES {
            body = body.push(
                container(small(
                    format!("{} more lines not shown.", lines.len() - FILE_LINES),
                    c,
                ))
                .padding([4, 8]),
            );
        }
        container(
            scrollable(body)
                .direction(scrollable::Direction::Both {
                    vertical: scrollable::Scrollbar::default(),
                    horizontal: scrollable::Scrollbar::default(),
                })
                .height(iced::Length::Shrink),
        )
        .max_height(360)
        .padding(iced::Padding {
            top: 2.0,
            right: 0.0,
            bottom: 6.0,
            left: 18.0,
        })
        .into()
    }

    /// What a merge needs, each said as done or as what is in the way.
    fn merge_checklist(
        &self,
        review: &Review,
        blockers: &[Blocker],
        target: &str,
        c: Colors,
    ) -> Element<'_, Message> {
        let check = |ok: bool, said: String| -> Element<'_, Message> {
            row![
                text(if ok { "✓" } else { "•" })
                    .size(12.5)
                    .color(if ok { c.green } else { c.amber })
                    .width(14),
                text(said)
                    .size(12.5)
                    .color(if ok { c.muted } else { c.text }),
            ]
            .spacing(6)
            .align_y(Center)
            .into()
        };
        let mut list = column![].spacing(3);
        list = list.push(
            match blockers
                .iter()
                .find(|b| matches!(b, Blocker::Uncommitted(_)))
            {
                Some(b) => check(false, b.label(target)),
                None => check(true, "work committed".to_owned()),
            },
        );
        list = list.push(
            match blockers
                .iter()
                .find(|b| matches!(b, Blocker::TargetDirty(_)))
            {
                Some(b) => check(false, b.label(target)),
                None => check(true, format!("{target} has nothing uncommitted")),
            },
        );
        list = list.push(
            match blockers.iter().find(|b| matches!(b, Blocker::Conflicts(_))) {
                Some(b) => check(
                    false,
                    format!("{}: {}", b.label(target), review.conflicts.join(", ")),
                ),
                None => check(true, "no conflicts".to_owned()),
            },
        );
        if let Some(b) = blockers.iter().find(|b| matches!(b, Blocker::Held(_))) {
            let who: Vec<String> = review
                .held
                .iter()
                .map(|(path, by)| format!("{by} holds {path}"))
                .collect();
            list = list.push(check(
                false,
                format!("{} ({})", b.label(target), who.join("; ")),
            ));
        }
        list.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unified_diff_splits_into_each_files_lines() {
        let patch = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/new.rs b/new.rs\nnew file mode 100644\n--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1 @@\n+fresh\n";
        let files = file_diffs(patch);
        assert_eq!(files["src/a.rs"], ["@@ -1 +1 @@", "-old", "+new"]);
        assert_eq!(files["new.rs"], ["@@ -0,0 +1 @@", "+fresh"]);
    }
}
