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
                return Some(error_line(
                    "changes-error",
                    &format!("Changes could not be read: {reason}"),
                    c,
                ));
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
        // A rebase leaves the main folder on no branch; it is still where
        // the merge would go.
        let target = review
            .target
            .clone()
            .unwrap_or_else(|| "the main folder".to_owned());
        let branch = review.branch.clone().unwrap_or_default();
        // Merged on GitHub: a squash or rebase merge leaves the branch's
        // own commits outside the target, so the review still counts
        // them. Only the same reviewed and current head is covered by that
        // merged pull request. Nothing is left to merge here, and the next step is the
        // cleanup, as the agent apps studied lead with once a pull request
        // has merged.
        let merged_upstream = self
            .pull_request_of(agent)
            .filter(|pull| pull.merged_head_matches(review.head.as_deref()))
            .filter(|_| {
                agent.vcs.as_ref().and_then(|vcs| vcs.head.as_deref()) == review.head.as_deref()
            })
            .map(|pull| pull.number);
        let (added, removed) = review.totals();
        let plural = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
        let mut section = column![
            row![
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
                format!("{branch} → {target}  +{added} −{removed}")
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
                // Done is a tick whose words (the restore command among
                // them) are its tooltip and name; a refusal is one line.
                match outcome {
                    Ok(words) => tip(
                        super::said(
                            format!("merge-outcome-{id}"),
                            words.clone(),
                            container(icon(Icon::Check, c.green, 14.0)).padding(3),
                        ),
                        words.clone(),
                        c,
                    ),
                    Err(reason) => error_line(format!("merge-outcome-{id}"), reason, c),
                }
            });
        // A detached rebase has no commit list. Show its blocker before
        // empty-branch messaging or cleanup controls, which would imply
        // that the worktree has been merged and is safe to remove.
        if review.in_progress.is_some() {
            if let Some(outcome) = outcome {
                section = section.push(outcome);
            }
            return Some(
                section
                    .push(self.merge_checklist(review, &blockers, &target, c))
                    .into(),
            );
        }
        if review.commits.is_empty() || merged_upstream.is_some() {
            if let Some(outcome) = outcome {
                section = section.push(outcome);
            }
            section = section.push(match merged_upstream {
                Some(number) => status_word(format!("Merged #{number}"), c.green, c),
                None => status_word("Up to date", c.green, c),
            });
            // Merged: the worktree has done its work. Removing it is the
            // next step and the person's call, here where they merged it;
            // a running agent keeps its folder until it is done.
            if review.checkout != review.target_checkout && !agent.status.is_live() {
                // Upstream configuration, not this review's commit list,
                // decides Git's branch deletion when it runs: the button
                // says it may delete the branch, and what runs is its
                // tooltip. The outcome then says what Git did.
                let removing = self.removing_worktree.as_deref() == Some(id.as_str());
                let worktree = review.checkout.display();
                section = section.push(tip(
                    action(
                        "remove-worktree",
                        if removing {
                            "Removing…"
                        } else {
                            "Remove worktree (may delete its branch)"
                        },
                        (!removing && self.connected.is_ok())
                            .then(|| Message::RemoveWorktree(id.clone())),
                        false,
                    ),
                    format!("git worktree remove {worktree} · git branch -d {branch}"),
                    c,
                ));
            }
            return Some(section.into());
        }
        let diffs = file_diffs(&review.patch);
        let mut files = column![].spacing(2).width(Fill);
        for file in &review.files {
            let open = self.shell.diff_file.as_deref() == Some(file.path.as_str());
            let counts = match (file.added, file.removed) {
                (Some(a), Some(r)) => format!("+{a} −{r}"),
                _ => String::new(),
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
        // The command it runs is its tooltip, so nothing about it is a
        // surprise.
        section = section.push(tip(
            primary(
                "merge-branch",
                if merging {
                    "Merging…".to_owned()
                } else {
                    format!("Merge into {target}")
                },
                ready.then(|| Message::MergeBranch(id.clone(), head)),
            ),
            format!("git merge --no-ff {branch}, in the {target} checkout"),
            c,
        ));
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
            // Too large to show is a mark that says so; a binary file or a
            // rename has nothing to show and shows nothing.
            return container(if review.truncated {
                super::alert_mark(
                    "diff-too-large",
                    "This diff is too large to show here; the file list is complete.",
                    c,
                )
            } else {
                Space::new().height(0).into()
            })
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
                container(
                    text(format!("+{}", lines.len() - FILE_LINES))
                        .size(11.5)
                        .font(Font::MONOSPACE)
                        .color(c.faint),
                )
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
        // Only what stops the merge is shown: one line each, the mark
        // and its words, none for what is fine.
        let blocked = |words: String| error_line("merge-blocker", &words, c);
        let mut list = column![].spacing(3);
        // The person's own operation under way in the main folder is the
        // one thing to say: nothing else was weighed while it lasts.
        if let Some(b) = blockers
            .iter()
            .find(|b| matches!(b, Blocker::InProgress(_)))
        {
            return list.push(blocked(b.label(target))).into();
        }
        for b in blockers.iter().filter(|b| {
            matches!(
                b,
                Blocker::Uncommitted(_)
                    | Blocker::TargetDirty(_)
                    | Blocker::Conflicts(_)
                    | Blocker::Held(_)
            )
        }) {
            list = list.push(blocked(match b {
                Blocker::Conflicts(_) => {
                    format!("{}: {}", b.label(target), review.conflicts.join(", "))
                }
                Blocker::Held(_) => {
                    let who: Vec<String> = review
                        .held
                        .iter()
                        .map(|(path, by)| format!("{by} holds {path}"))
                        .collect();
                    format!("{} ({})", b.label(target), who.join("; "))
                }
                _ => b.label(target),
            }));
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
        let merged_head = "1111111111111111111111111111111111111111";
        let new_head = "2222222222222222222222222222222222222222";
        let mut agent = crate::app::tests::record("feature", "claude-code", None);
        agent.status = agentdocker_core::AgentStatus::Exited { code: Some(0) };
        agent.project = Some(agentdocker_core::ProjectRef::directory(root));
        agent.vcs = Some(agentdocker_core::VcsState {
            branch: Some("claude/feature".into()),
            head: Some(merged_head.into()),
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
                head: Some(merged_head.into()),
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
                    head: Some(merged_head.into()),
                })),
            )],
            std::time::Instant::now(),
        );
        let after = seen(app.changes_section(&agent, c).expect("changes"));
        assert!(after.control("merge-branch").is_none());
        assert!(after.reads("Merged #340"));
        // A squash and an empty commit list cannot predict branch -d: an
        // upstream can make Git delete either branch when it runs. What
        // runs is the button's tooltip, not a line under it.
        let remove = after.control("remove-worktree").expect("remove");
        assert_eq!(remove.1, "Remove worktree (may delete its branch)");
        assert!(!after.reads("git worktree remove"));
        let merged_commits = app
            .branch_review
            .as_ref()
            .unwrap()
            .1
            .as_ref()
            .unwrap()
            .commits
            .clone();
        if let Some((_, Ok(review))) = app.branch_review.as_mut() {
            review.commits.clear();
        }
        let clean = seen(app.changes_section(&agent, c).expect("changes"));
        assert_eq!(
            clean.control("remove-worktree").expect("remove").1,
            "Remove worktree (may delete its branch)"
        );
        let _ = app.update(Message::RemoveWorktree(agent.id.to_string()));
        let actual =
            "Deleted branch renamed-after-review (was 1234567890abcdef1234567890abcdef12345678).";
        _messages
            .send(crate::app::Msg::WorktreeRemoved(
                agent.id.to_string(),
                Ok((true, Some(actual.into()))),
            ))
            .unwrap();
        app.drain();
        let said = app.merge_outcome.as_ref().unwrap().1.as_ref().unwrap();
        assert_eq!(said, &format!("Worktree and branch removed. {actual}"));
        assert!(!said.contains("claude/feature"));
        assert!(!said.contains(merged_head));

        // Older daemons may omit the note; no cached identity is substituted.
        _messages
            .send(crate::app::Msg::WorktreeRemoved(
                agent.id.to_string(),
                Ok((true, None)),
            ))
            .unwrap();
        app.drain();
        assert_eq!(
            app.merge_outcome.as_ref().unwrap().1.as_deref(),
            Ok("Worktree and branch removed.")
        );
        _messages
            .send(crate::app::Msg::WorktreeRemoved(
                agent.id.to_string(),
                Ok((
                    false,
                    Some("branch actual is kept: Git refused deletion".into()),
                )),
            ))
            .unwrap();
        app.drain();
        assert_eq!(
            app.merge_outcome.as_ref().unwrap().1.as_deref(),
            Ok(
                "Worktree removed; its branch was not deleted. branch actual is kept: Git refused deletion"
            )
        );

        app.branch_review
            .as_mut()
            .unwrap()
            .1
            .as_mut()
            .unwrap()
            .commits = merged_commits;
        // A new registry head must stop offering cleanup even before the
        // refreshed branch review arrives. The cached PR is still merged.
        agent.vcs.as_mut().unwrap().head = Some(new_head.into());
        let moved = seen(app.changes_section(&agent, c).expect("changes"));
        assert!(moved.control("merge-branch").is_some());
        assert!(moved.control("remove-worktree").is_none());
        if let Some((_, Ok(review))) = app.branch_review.as_mut() {
            review.head = Some(new_head.into());
        }
        let refreshed = seen(app.changes_section(&agent, c).expect("changes"));
        assert!(refreshed.control("merge-branch").is_some());
        assert!(refreshed.control("remove-worktree").is_none());
        assert!(!refreshed.reads("Merged #340"));
    }

    #[test]
    fn a_unified_diff_splits_into_each_files_lines() {
        let patch = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/new.rs b/new.rs\nnew file mode 100644\n--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1 @@\n+fresh\n";
        let files = file_diffs(patch);
        assert_eq!(files["src/a.rs"], ["@@ -1 +1 @@", "-old", "+new"]);
        assert_eq!(files["new.rs"], ["@@ -0,0 +1 @@", "+fresh"]);
    }
}
