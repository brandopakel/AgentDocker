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
        // Merged on GitHub: a squash or rebase merge leaves the branch's
        // own commits outside the target, so the review still counts
        // them. Nothing is left to merge here, and the next step is the
        // cleanup, as the agent apps studied lead with once a pull request
        // has merged.
        let merged_upstream = self
            .pull_request_of(agent)
            .filter(|pull| pull.status == crate::pull_request::Status::Merged)
            .map(|pull| pull.number);
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
            text(if review.commits.is_empty() || merged_upstream.is_some() {
                format!("{branch} → {target}")
            } else {
                format!(
                    "{branch} → {target} · {} · {} · +{added} −{removed}",
                    plural(review.commits.len(), "commit"),
                    plural(review.files.len(), "file"),
                )
            })
            .size(13)
            .color(c.text),
        ]
        .spacing(8)
        .width(Fill);
        // What the last merge said stays under the section, also once
        // there is nothing left to merge.
        let outcome = self
            .merge_outcome
            .as_ref()
            .filter(|(outcome_for, _)| outcome_for == id)
            .map(|(_, outcome)| -> Element<'_, Message> {
                match outcome {
                    Ok(said) => text(said.clone()).size(12.5).color(c.green).into(),
                    Err(reason) => text(reason.clone()).size(12.5).color(c.amber).into(),
                }
            });
        if review.commits.is_empty() || merged_upstream.is_some() {
            if let Some(outcome) = outcome {
                section = section.push(outcome);
            }
            section = section.push(small(
                match merged_upstream {
                    Some(number) => format!("Merged on GitHub as #{number}."),
                    None => format!("Nothing on {branch} that {target} does not have."),
                },
                c,
            ));
            // Merged: the worktree has done its work. Removing it is the
            // next step and the person's call, here where they merged it;
            // a running agent keeps its folder until it is done.
            if review.checkout != review.target_checkout {
                if agent.status.is_live() {
                    section = section.push(small(
                        format!(
                            "Its worktree stays while {} runs; remove it here once it is done.",
                            self.display_name(agent)
                        ),
                        c,
                    ));
                } else {
                    // Git deletes the branch only when it sees it merged:
                    // with nothing left on it, it does; after a squash on
                    // GitHub, it keeps it. The button says which before it
                    // is pressed, as Tower's "Safe to delete" does.
                    let keeps = !review.commits.is_empty();
                    let removing = self.removing_worktree.as_deref() == Some(id.as_str());
                    section = section.push(action(
                        "remove-worktree",
                        &if removing {
                            "Removing…".to_owned()
                        } else if keeps {
                            format!("Remove worktree (keeps {branch})")
                        } else {
                            "Remove worktree and branch".to_owned()
                        },
                        (!removing && self.connected.is_ok())
                            .then(|| Message::RemoveWorktree(id.clone())),
                        false,
                    ));
                    // What it runs, as Merge says what it runs.
                    let worktree = review.checkout.display();
                    section = section.push(
                        text(if keeps {
                            format!(
                                "git worktree remove {worktree} · git keeps {branch}: it does not see the squash as merged"
                            )
                        } else {
                            format!("git worktree remove {worktree} · git branch -d {branch}")
                        })
                        .size(11.5)
                        .font(Font::MONOSPACE)
                        .color(c.faint),
                    );
                }
            }
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
        // What only the agent can fix goes back to it as a message: its own
        // uncommitted work, or a conflict on its branch. The person's own
        // checkout and other agents' leases stay theirs to sort out.
        let name = self.display_name(agent);
        let mut asks = row![].spacing(8).align_y(Center);
        let live = agent.status.is_live() && self.connected.is_ok();
        if let Some(Blocker::Uncommitted(n)) = blockers
            .iter()
            .find(|b| matches!(b, Blocker::Uncommitted(_)))
        {
            asks = asks.push(action(
                "ask-commit",
                format!("Ask {name} to commit"),
                live.then(|| {
                    Message::AskAgent(
                        id.clone(),
                        format!(
                            "{} on {branch} {} not committed. Please commit your work so it can be merged into {target}, then tell me.",
                            plural(*n, "file"),
                            if *n == 1 { "is" } else { "are" },
                        ),
                    )
                }),
                false,
            ));
        }
        if blockers.iter().any(|b| matches!(b, Blocker::Conflicts(_))) {
            asks = asks.push(action(
                "ask-resolve",
                format!("Ask {name} to resolve"),
                live.then(|| {
                    Message::AskAgent(
                        id.clone(),
                        format!(
                            "Merging {branch} into {target} would conflict in {}. Please bring {target} into your branch (git merge {target}), resolve the conflicts, commit, and tell me when it is done.",
                            review.conflicts.join(", "),
                        ),
                    )
                }),
                false,
            ));
        }
        if let Some(pull) = self.pull_request_of(agent)
            && pull.status == crate::pull_request::Status::Failed
        {
            asks = asks.push(action(
                "ask-fix-checks",
                format!("Ask {name} to fix checks"),
                live.then(|| {
                    Message::AskAgent(
                        id.clone(),
                        format!(
                            "The checks on pull request #{} failed. Please look at the failures (gh pr checks {}), fix them, push, and tell me when they pass.",
                            pull.number, pull.number,
                        ),
                    )
                }),
                false,
            ));
        }
        section = section.push(asks.wrap());
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
        // The command it runs, so nothing about it is a surprise.
        section = section.push(
            text(format!(
                "git merge --no-ff {branch}, in the {target} checkout"
            ))
            .size(11.5)
            .font(Font::MONOSPACE)
            .color(c.faint),
        );
        if let Some(outcome) = outcome {
            section = section.push(outcome);
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
                        .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
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
        container(scrollable(body).height(iced::Length::Shrink))
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
    use super::super::testing::seen;
    use super::*;

    /// A pull request merged on GitHub leaves the branch's commits outside
    /// the target after a squash; Changes then offers no merge and leads
    /// with removing the worktree.
    #[test]
    fn a_pull_request_merged_on_github_leads_with_the_cleanup() {
        use crate::pull_request::{Key, PullRequest, Status};
        let (commands, _requests) = crate::app::queue::channel();
        let (_messages, results) = std::sync::mpsc::sync_channel(crate::app::MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        app.connected = Ok(());
        let root = std::path::Path::new("/fixture/alpha");
        let mut agent = crate::app::tests::record("feature", "claude-code", None);
        agent.status = agentdocker_core::AgentStatus::Exited { code: Some(0) };
        agent.project = Some(agentdocker_core::ProjectRef::directory(root));
        agent.vcs = Some(agentdocker_core::VcsState {
            branch: Some("claude/feature".into()),
            head: None,
            dirty: None,
            updated_at: Utc::now(),
        });
        app.agents.push(agent.clone());
        app.branch_review = Some((
            agent.id.to_string(),
            Ok(Review {
                agent: agent.id.clone(),
                checkout: "/fixture/alpha-wt/feature".into(),
                branch: Some("claude/feature".into()),
                target_checkout: root.into(),
                target: Some("main".into()),
                commits: vec![agentdocker_core::review::Commit {
                    sha: "abc1234".into(),
                    subject: "Do the thing".into(),
                }],
                ..Review::default()
            }),
        ));
        let c = style::Colors::new(true);
        let before = seen(app.changes_section(&agent, c).expect("changes"));
        assert!(before.control("merge-branch").is_some(), "not merged yet");
        assert!(before.control("remove-worktree").is_none());

        app.shell.pull_requests.answered(
            vec![(
                Key::new(root, "claude/feature").unwrap(),
                Ok(Some(PullRequest {
                    number: 340,
                    url: "https://github.com/o/n/pull/340".into(),
                    status: Status::Merged,
                })),
            )],
            std::time::Instant::now(),
        );
        let after = seen(app.changes_section(&agent, c).expect("changes"));
        assert!(after.control("merge-branch").is_none());
        assert!(after.reads("Merged on GitHub as #340."));
        // Git does not see a squash as merged: the button says the branch
        // stays, and the line under it says what runs.
        let remove = after.control("remove-worktree").expect("remove");
        assert_eq!(remove.1, "Remove worktree (keeps claude/feature)");
        assert!(after.reads("git worktree remove /fixture/alpha-wt/feature"));

        // With nothing left on the branch, git deletes it too; once it
        // has, the outcome says how to bring it back.
        if let Some((_, Ok(review))) = app.branch_review.as_mut() {
            review.commits.clear();
            review.head = Some("1a2b3c4d5e6f".into());
        }
        let clean = seen(app.changes_section(&agent, c).expect("changes"));
        assert_eq!(
            clean.control("remove-worktree").expect("remove").1,
            "Remove worktree and branch"
        );
        assert!(clean.reads("git branch -d claude/feature"));
        let _ = app.update(Message::RemoveWorktree(agent.id.to_string()));
        _messages
            .send(crate::app::Msg::WorktreeRemoved(
                agent.id.to_string(),
                Ok(true),
            ))
            .unwrap();
        app.drain();
        let said = app
            .merge_outcome
            .as_ref()
            .and_then(|(_, said)| said.as_ref().ok());
        assert_eq!(
            said.map(String::as_str),
            Some(
                "Worktree and branch removed. claude/feature was at 1a2b3c4: git branch claude/feature 1a2b3c4 brings it back."
            )
        );
    }

    #[test]
    fn a_unified_diff_splits_into_each_files_lines() {
        let patch = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/new.rs b/new.rs\nnew file mode 100644\n--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1 @@\n+fresh\n";
        let files = file_diffs(patch);
        assert_eq!(files["src/a.rs"], ["@@ -1 +1 @@", "-old", "+new"]);
        assert_eq!(files["new.rs"], ["@@ -0,0 +1 @@", "+fresh"]);
    }
}
