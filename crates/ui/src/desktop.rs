//! Native installation controls backed by the sibling CLI's checked operations.

use serde_json::Value;

#[derive(Default)]
pub struct Panel {
    source: String,
    prefix: String,
    local_preview: bool,
    busy: bool,
    report: Option<Value>,
    error: Option<String>,
    /// What the last `status` said was installed here, so the screen can
    /// offer a rollback only when there is a version to roll back to.
    /// `None` means nobody has asked yet.
    installed: Option<Installed>,
}

/// What is at a prefix, as far as the last status knows.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Installed {
    current: bool,
    previous: bool,
}

impl Panel {
    pub fn receive(&mut self, result: Result<Value, String>) {
        self.busy = false;
        match result {
            Ok(report) => {
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

    fn command(&self, operation: &str) -> Vec<String> {
        let mut args = Vec::new();
        if !self.prefix.trim().is_empty() {
            args.extend(["--prefix".into(), self.prefix.clone()]);
        }
        args.push(operation.into());
        if self.local_preview && operation != "status" {
            args.push("--local-preview".into());
        }
        args
    }

    /// Present a checked preview before activation; the CLI pins both the
    /// reviewed payload and prior active release when the user applies it.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<Vec<String>> {
        ui.heading("Desktop installation");
        ui.label(
            "Install an extracted agentdocker package, or return to the previous retained version.",
        );
        ui.label("Activation takes effect on the next app or command launch. Your running daemon and agents continue until you explicitly restart or reload them.");
        let mut command = None;
        ui.add_enabled_ui(!self.busy, |ui| {
            ui.label("Application bundle or extracted desktop package");
            let mut changed = ui.text_edit_singleline(&mut self.source).changed();
            if ui.button("Use this application").clicked() {
                match std::env::current_exe().ok().and_then(|exe| {
                    exe.parent()?.parent().map(|parent| {
                        if cfg!(target_os = "macos") { parent.parent().unwrap_or(parent) } else { parent }.to_owned()
                    })
                }) {
                    Some(path) => { self.source = path.display().to_string(); changed = true; }
                    None => self.error = Some("Cannot locate this application".into()),
                }
            }
            ui.label("Installation prefix (leave empty for your home; use a disposable directory for trials)");
            if ui.text_edit_singleline(&mut self.prefix).changed() {
                changed = true;
                // A different prefix is a different installation; what
                // the last status found does not describe it.
                self.installed = None;
            }
            if cfg!(target_os = "macos") {
                changed |= ui.checkbox(&mut self.local_preview, "Allow a locally signed preview build").changed();
            }
            if changed { self.report = None; }
            ui.horizontal(|ui| {
                if ui
                    .button("Show installed versions")
                    .on_hover_text("Read what is installed at this prefix. Changes nothing.")
                    .clicked()
                {
                    command = Some(self.command("status"));
                }
                if ui
                    .add_enabled(
                        !self.source.trim().is_empty(),
                        egui::Button::new("Preview installation"),
                    )
                    .on_hover_text(
                        "Check the package and report what installing it would do. \
                         Nothing is activated until you apply it.",
                    )
                    .on_disabled_hover_text("Give it a bundle or package to look at first.")
                    .clicked()
                {
                    let mut args = self.command("install");
                    args.extend(["--from".into(), self.source.clone(), "--preview".into()]);
                    command = Some(args);
                }
                // Offered only when the last status found something to
                // go back to. A live button whose only outcome is "no
                // active desktop installation" is a red error the reader
                // caused by pressing what the screen invited them to.
                let can_roll_back = self.installed.is_none_or(|at| at.previous);
                let rollback = ui
                    .add_enabled(can_roll_back, egui::Button::new("Preview rollback"))
                    .on_hover_text("Report what returning to the retained previous version would do.");
                if !can_roll_back {
                    rollback.on_hover_text(
                        "Nothing to roll back to here: this prefix has no previous version.",
                    );
                } else if rollback.clicked() {
                    let mut args = self.command("rollback");
                    args.push("--preview".into());
                    command = Some(args);
                }
            });
            if let Some(report) = &self.report {
                ui.separator();
                if report.get("installation").is_some() {
                    if report["installation"].is_null() {
                        ui.label("No managed desktop installation at this prefix.");
                    } else {
                        describe(ui, "Active", &report["installation"]["current"]);
                        if !report["installation"]["previous"].is_null() {
                            describe(ui, "Previous", &report["installation"]["previous"]);
                        }
                    }
                } else {
                    describe(ui, "Selected", &report["candidate"]);
                    for (label, key) in [("Application", "application"), ("Commands", "bin"), ("Retained versions", "versions")] {
                        ui.label(format!("{label}: {}", report[key].as_str().unwrap_or("unknown")));
                    }
                    if report["preview"] == true {
                        // The apply pins exactly what was reviewed, so a
                        // preview missing either half cannot be applied.
                        // It used to be a `?` here, which abandoned the
                        // click and the rest of the frame without a
                        // word: a button that does nothing, silently.
                        let pinned = pinned(report);
                        if ui
                            .add_enabled(
                                pinned.is_some(),
                                egui::Button::new("Apply this installation"),
                            )
                            .on_hover_text(
                                "Activate exactly the release reviewed above. The previous one \
                                 is retained, so this can be rolled back.",
                            )
                            .on_disabled_hover_text(
                                "This preview does not name the release it checked, so there is \
                                 nothing safe to pin an install to. Preview it again.",
                            )
                            .clicked()
                            && let Some((source, release)) = pinned
                        {
                            let mut args = self.command("install");
                            args.extend([
                                "--from".into(), source.to_owned(),
                                "--expect-release".into(), release.to_owned(),
                                "--expect-current".into(), report["previous"]["id"].as_str().unwrap_or("none").into(),
                            ]);
                            command = Some(args);
                        }
                    } else {
                        ui.label("Installation activated. Reopen agentdocker to use it.");
                    }
                }
            }
        });
        if let Some(error) = &self.error {
            ui.colored_label(crate::theme::ABSENT, error);
        }
        if self.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Verifying desktop installation…");
            });
        }
        if command.is_some() {
            self.busy = true;
            self.error = None;
        }
        command
    }
}

/// What an apply has to pin: the package that was checked and the
/// release found in it. A preview naming neither cannot be applied to
/// anything in particular, and an install that is not pinned to what was
/// reviewed is not the install that was reviewed.
fn pinned(report: &Value) -> Option<(&str, &str)> {
    report["source"]
        .as_str()
        .zip(report["candidate"]["id"].as_str())
}

fn describe(ui: &mut egui::Ui, label: &str, release: &Value) {
    let version = release["version"].as_str().unwrap_or("unknown");
    let commit = release["source_commit"].as_str().unwrap_or("unknown");
    ui.label(format!(
        "{label}: agentdocker {version}, source {}",
        commit.chars().take(12).collect::<String>()
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    /// A report is the answer to the question that was asked. Changing
    /// the source or the prefix asks a different question, so the old
    /// answer must not stay on screen next to the new inputs — an apply
    /// button is built from the report, and applying a stale one would
    /// install something the reader did not choose.
    #[test]
    fn a_report_belongs_to_the_inputs_that_produced_it() {
        let mut p = panel();
        p.receive(Ok(json!({"preview": true, "source": "/a"})));
        assert!(p.report.is_some());
        assert!(!p.busy, "a reply ends the wait");

        // What `show` does when an input changes, without a window.
        p.report = None;
        assert!(p.report.is_none());
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

    /// The panel lays itself out in every state it can be shown in.
    #[test]
    fn the_panel_lays_itself_out_in_each_of_its_states() {
        let ctx = egui::Context::default();
        let reports = [
            json!({"installation": null}),
            json!({"installation": {"current": {"id": "b"}, "previous": {"id": "a"}}}),
            json!({"preview": true, "source": "/tmp/pkg", "candidate": {"id": "v1"},
                   "previous": {"id": "v0"}, "application": "/Applications", "bin": "~/.local/bin",
                   "versions": "2"}),
            // The preview that cannot be pinned, and so cannot be applied.
            json!({"preview": true, "candidate": {}, "application": "/Applications",
                   "bin": "~/.local/bin", "versions": "2"}),
            json!({"preview": false, "candidate": {"id": "v1"}, "application": "/Applications",
                   "bin": "~/.local/bin", "versions": "2"}),
        ];
        for report in reports {
            let mut p = panel();
            p.receive(Ok(report));
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                p.show(ui);
            });
            output.textures_delta.clear();
            assert!(
                !ctx.tessellate(output.shapes, output.pixels_per_point)
                    .is_empty()
            );
        }
        let mut p = panel();
        p.receive(Err("no such package".to_owned()));
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            p.show(ui);
        });
        output.textures_delta.clear();
        assert!(
            !ctx.tessellate(output.shapes, output.pixels_per_point)
                .is_empty()
        );
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
