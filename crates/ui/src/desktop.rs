//! Native installation controls backed by the sibling CLI's checked operations.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UpdateSchedule {
    pub enabled: bool,
    pub last_attempt: Option<i64>,
}

impl UpdateSchedule {
    pub fn due(&self, now: i64) -> bool {
        self.enabled
            && self
                .last_attempt
                .is_none_or(|last| now.saturating_sub(last) >= 24 * 60 * 60)
    }
}

#[derive(Default)]
pub struct Panel {
    pub source: String,
    pub prefix: String,
    pub local_preview: bool,
    pub busy: bool,
    pub report: Option<Value>,
    pub error: Option<String>,
    pub checking_updates: bool,
    pub update_check_error: bool,
    pub update: Option<Value>,
    /// What the last `status` said was installed here, so the screen can
    /// offer a rollback only when there is a version to roll back to.
    /// `None` means nobody has asked yet.
    pub installed: Option<Installed>,
}

/// What is at a prefix, as far as the last status knows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Installed {
    pub current: bool,
    pub previous: bool,
}

impl Panel {
    pub fn preview(&self, operation: &str) -> Option<Vec<String>> {
        if self.busy
            || (operation == "update-check" && self.checking_updates)
            || !matches!(
                operation,
                "status"
                    | "install"
                    | "rollback"
                    | "uninstall"
                    | "prune"
                    | "update-check"
                    | "update"
            )
        {
            return None;
        }
        // Updates come from the published feed, not from a typed source. A
        // check downloads nothing; `update` downloads, verifies and previews,
        // and the ordinary Apply then pins what was previewed.
        if operation == "update-check" {
            let mut args = self.command("update");
            args.push("--check".into());
            return Some(args);
        }
        if operation == "update" {
            return self.update_available().map(|_| self.command("update"));
        }
        let mut args = self.command(operation);
        if operation == "install" {
            if self.source.trim().is_empty() {
                return None;
            }
            args.extend(["--from".into(), self.source.clone()]);
        }
        if operation == "rollback" && self.installed.is_some_and(|at| !at.previous) {
            return None;
        }
        if operation != "status" {
            args.push("--preview".into());
        }
        Some(args)
    }

    pub fn apply(&self) -> Option<Vec<String>> {
        if self.busy {
            return None;
        }
        let report = self.report.as_ref()?;
        if report["preview"] != true {
            return None;
        }
        if report.get("maintenance").is_some() {
            let (operation, id) = pinned_maintenance(report)?;
            let mut args = self.command(operation);
            args.extend(["--expect-plan".into(), id.into()]);
            if let Some(keep) = report["maintenance"]["keep"].as_u64() {
                args.extend(["--keep".into(), keep.to_string()]);
            }
            Some(args)
        } else {
            let (source, release) = pinned(report)?;
            let mut args = self.command("install");
            args.extend([
                "--from".into(),
                source.into(),
                "--expect-release".into(),
                release.into(),
                "--expect-current".into(),
                report["previous"]["id"].as_str().unwrap_or("none").into(),
            ]);
            Some(args)
        }
    }
    pub fn receive(&mut self, result: Result<Value, String>) {
        self.busy = false;
        match result {
            Ok(report) => {
                if let Some(update) = report.get("update") {
                    self.update = Some(update.clone());
                    self.update_check_error = false;
                } else if report["preview"] == false && report.get("candidate").is_some() {
                    self.update = None;
                }
                if let Some(installation) = report.get("installation") {
                    self.installed = Some(Installed {
                        current: !installation["current"].is_null(),
                        previous: !installation["previous"].is_null(),
                    });
                }
                self.report = Some(report);
                self.error = None;
            }
            Err(error) => {
                self.report = None;
                self.error = Some(error);
            }
        }
    }

    /// Background checks have their own result; an installation preview and
    /// its Apply pin remain exactly the operation the user reviewed.
    pub fn receive_update(&mut self, result: Result<Value, String>) {
        self.checking_updates = false;
        match result.ok().and_then(|report| report.get("update").cloned()) {
            Some(update) => {
                self.update = Some(update);
                self.update_check_error = false;
            }
            None => self.update_check_error = true,
        }
    }

    pub fn command(&self, operation: &str) -> Vec<String> {
        let mut args = Vec::new();
        if !self.prefix.trim().is_empty() {
            args.extend(["--prefix".into(), self.prefix.clone()]);
        }
        args.push(operation.into());
        if self.local_preview && matches!(operation, "install" | "rollback" | "update") {
            args.push("--local-preview".into());
        }
        args
    }

    /// The newer version the last check or preview found, if any.
    pub fn update_available(&self) -> Option<&str> {
        let update = self
            .update
            .as_ref()
            .or_else(|| self.report.as_ref()?.get("update"))?;
        (update["update_available"] == true)
            .then(|| update["available"]["version"].as_str())
            .flatten()
    }
}

/// What an apply has to pin: the package that was checked and the
/// release found in it. A preview naming neither cannot be applied to
/// anything in particular, and an install that is not pinned to what was
/// reviewed is not the install that was reviewed.
pub fn pinned(report: &Value) -> Option<(&str, &str)> {
    report["source"]
        .as_str()
        .zip(report["candidate"]["id"].as_str())
}

pub fn pinned_maintenance(report: &Value) -> Option<(&str, &str)> {
    let operation = report["maintenance"]["operation"].as_str()?;
    let plan_id = report["plan_id"].as_str()?;
    (matches!(operation, "prune" | "uninstall") && !plan_id.is_empty())
        .then_some((operation, plan_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn daily_checks_require_opt_in_and_survive_clock_rollback_and_restart() {
        let mut schedule = UpdateSchedule::default();
        assert!(!schedule.due(100_000));
        schedule.enabled = true;
        assert!(schedule.due(100_000));
        schedule.last_attempt = Some(100_000);
        let restored: UpdateSchedule =
            serde_json::from_str(&serde_json::to_string(&schedule).unwrap()).unwrap();
        assert!(!restored.due(99_999));
        assert!(!restored.due(186_399));
        assert!(restored.due(186_400));
    }

    #[test]
    fn background_results_preserve_installation_preview_pin_and_busy_state() {
        let report = json!({"preview":true,"source":"/fixture","candidate":{"id":"reviewed"}});
        let mut panel = Panel {
            report: Some(report.clone()),
            ..Default::default()
        };
        let apply = panel.apply();
        panel.checking_updates = true;
        panel.receive_update(Ok(
            json!({"update":{"update_available":true,"available":{"version":"0.2.0"}}}),
        ));
        assert_eq!(panel.update_available(), Some("0.2.0"));
        assert_eq!(panel.apply(), apply);
        panel.busy = true;
        panel.receive_update(Err("fixture network failure".into()));
        assert!(panel.busy && panel.update_check_error);
        assert_eq!(panel.report, Some(report));
        assert!(!panel.checking_updates);
    }

    #[test]
    fn update_check_needs_no_source_and_update_needs_a_found_version() {
        let idle = Panel::default();
        assert_eq!(
            idle.preview("update-check"),
            Some(vec!["update".to_owned(), "--check".to_owned()])
        );
        assert_eq!(idle.preview("update"), None, "nothing found yet");
        let nothing_newer = Panel {
            report: Some(
                json!({"update": {"update_available": false, "available": {"version": "0.1.0"}}}),
            ),
            ..Panel::default()
        };
        assert_eq!(nothing_newer.update_available(), None);
        assert_eq!(nothing_newer.preview("update"), None);
        let found = Panel {
            report: Some(
                json!({"update": {"update_available": true, "available": {"version": "0.2.0"}}}),
            ),
            local_preview: true,
            ..Panel::default()
        };
        assert_eq!(found.update_available(), Some("0.2.0"));
        assert_eq!(
            found.preview("update"),
            Some(vec!["update".to_owned(), "--local-preview".to_owned()])
        );
        let busy = Panel {
            busy: true,
            ..Panel::default()
        };
        assert_eq!(busy.preview("update-check"), None);
    }

    #[test]
    fn a_downloaded_update_preview_applies_through_the_ordinary_pin() {
        let p = Panel {
            report: Some(
                json!({"preview": true, "source": "/tmp/payload/AgentDocker.app",
                "candidate": {"id": "abc"}, "previous": {"id": "old"},
                "update": {"update_available": true, "available": {"version": "0.2.0"}}}),
            ),
            ..Panel::default()
        };
        assert_eq!(
            p.apply(),
            Some(vec![
                "install".to_owned(),
                "--from".to_owned(),
                "/tmp/payload/AgentDocker.app".to_owned(),
                "--expect-release".to_owned(),
                "abc".to_owned(),
                "--expect-current".to_owned(),
                "old".to_owned()
            ])
        );
    }

    fn panel() -> Panel {
        Panel::default()
    }

    /// The prefix is what makes a trial disposable: without it every
    /// preview and apply writes into the reader's real home. It has to
    /// reach the CLI on every operation, not just the ones that write.
    #[test]
    fn a_prefix_reaches_every_operation() {
        let mut p = panel();
        p.prefix = "/tmp/trial".into();
        for operation in ["status", "install", "rollback"] {
            let args = p.command(operation);
            assert_eq!(
                args.iter().position(|a| a == "--prefix"),
                Some(0),
                "the prefix leads, before the subcommand: {args:?}"
            );
            assert_eq!(args[1], "/tmp/trial");
            assert_eq!(args[2], operation);
        }
    }

    #[test]
    fn no_prefix_means_no_flag_rather_than_an_empty_one() {
        let mut p = panel();
        p.prefix = "   ".into();
        assert_eq!(p.command("status"), vec!["status".to_owned()]);
    }

    /// `--local-preview` relaxes which signatures are accepted, so it
    /// must never ride along on a read-only query. `status` answering
    /// differently depending on a checkbox would be a lie about what is
    /// installed.
    #[test]
    fn a_locally_signed_build_is_allowed_for_writes_only() {
        let mut p = panel();
        p.local_preview = true;
        assert!(!p.command("status").contains(&"--local-preview".to_owned()));
        for operation in ["install", "rollback"] {
            assert!(
                p.command(operation).contains(&"--local-preview".to_owned()),
                "{operation} carries it"
            );
        }
    }

    /// Receiving a preview stores its report and ends the busy state.
    #[test]
    fn receiving_a_report_finishes_the_busy_state() {
        let mut p = panel();
        p.receive(Ok(json!({"preview": true, "source": "/a"})));
        assert!(p.report.is_some());
        assert!(!p.busy, "a reply ends the wait");
    }

    /// A live button whose only outcome is an error is a fault the
    /// screen invited. Rollback is offered when there is a version to
    /// go back to, and only then.
    #[test]
    fn rollback_is_offered_only_when_there_is_something_to_roll_back_to() {
        let mut p = panel();
        assert!(
            p.installed.is_none(),
            "before anyone asks, nothing is known"
        );

        p.receive(Ok(json!({"installation": null})));
        assert_eq!(
            p.installed.map(|at| at.previous),
            Some(false),
            "an empty prefix has no previous version"
        );

        p.receive(Ok(
            json!({"installation": {"current": {"id": "a"}, "previous": null}}),
        ));
        assert_eq!(
            p.installed.map(|at| at.previous),
            Some(false),
            "one version is not two"
        );

        p.receive(Ok(json!({
            "installation": {"current": {"id": "b"}, "previous": {"id": "a"}}
        })));
        assert_eq!(
            p.installed.map(|at| at.previous),
            Some(true),
            "now there is"
        );

        // A report that is not a status says nothing about the prefix
        // and must not overwrite what a status found.
        p.receive(Ok(json!({"candidate": {"id": "c"}, "preview": true})));
        assert_eq!(p.installed.map(|at| at.previous), Some(true));
    }

    /// An install is offered only when it can be pinned to what was
    /// reviewed.
    ///
    /// This was a `?` inside the panel's closure, which abandoned the
    /// click and the rest of the frame without saying anything: a button
    /// that did nothing when pressed, which to the person pressing it is
    /// a button that does not work.
    #[test]
    fn an_apply_is_offered_only_when_it_can_be_pinned_to_what_was_reviewed() {
        assert_eq!(
            pinned(&json!({"source": "/tmp/pkg", "candidate": {"id": "v1"}})),
            Some(("/tmp/pkg", "v1"))
        );
        assert!(pinned(&json!({"candidate": {"id": "v1"}})).is_none());
        assert!(pinned(&json!({"source": "/tmp/pkg", "candidate": {}})).is_none());
        assert!(pinned(&json!({"source": 7, "candidate": {"id": "v1"}})).is_none());
        assert!(pinned(&json!({})).is_none());
    }

    #[test]
    fn cleanup_requires_a_known_operation_and_reviewed_plan() {
        for operation in ["prune", "uninstall"] {
            assert_eq!(
                pinned_maintenance(
                    &json!({"maintenance":{"operation":operation},"plan_id":"reviewed"})
                ),
                Some((operation, "reviewed"))
            );
        }
        for report in [
            json!({}),
            json!({"maintenance":{"operation":"prune"}}),
            json!({"maintenance":{"operation":"prune"},"plan_id":""}),
            json!({"maintenance":{"operation":"install"},"plan_id":"reviewed"}),
            json!({"maintenance":{"operation":7},"plan_id":"reviewed"}),
        ] {
            assert!(pinned_maintenance(&report).is_none());
        }
    }

    #[test]
    fn a_failure_replaces_the_report_rather_than_sitting_beside_it() {
        let mut p = panel();
        p.receive(Ok(json!({"preview": true})));
        p.busy = true;
        p.receive(Err("the payload is not a desktop artifact".into()));
        assert!(p.report.is_none(), "no stale report under a fresh error");
        assert_eq!(
            p.error.as_deref(),
            Some("the payload is not a desktop artifact")
        );
        assert!(!p.busy);

        // And a later success clears the error, or the reader sees a
        // complaint about something that has since worked.
        p.receive(Ok(json!({"preview": false})));
        assert!(p.error.is_none());
        assert!(p.report.is_some());
    }
}
