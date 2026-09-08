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
}

impl Panel {
    pub fn receive(&mut self, result: Result<Value, String>) {
        self.busy = false;
        match result {
            Ok(report) => {
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
            changed |= ui.text_edit_singleline(&mut self.prefix).changed();
            if cfg!(target_os = "macos") {
                changed |= ui.checkbox(&mut self.local_preview, "Allow a locally signed preview build").changed();
            }
            if changed { self.report = None; }
            ui.horizontal(|ui| {
                if ui.button("Show installed versions").clicked() {
                    command = Some(self.command("status"));
                }
                if ui.add_enabled(!self.source.trim().is_empty(), egui::Button::new("Preview installation")).clicked() {
                    let mut args = self.command("install");
                    args.extend(["--from".into(), self.source.clone(), "--preview".into()]);
                    command = Some(args);
                }
                if ui.button("Preview rollback").clicked() {
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
                        if ui.button("Apply this installation").clicked() {
                            let mut args = self.command("install");
                            args.extend([
                                "--from".into(), report["source"].as_str()?.into(),
                                "--expect-release".into(), report["candidate"]["id"].as_str()?.into(),
                                "--expect-current".into(), report["previous"]["id"].as_str().unwrap_or("none").into(),
                            ]);
                            command = Some(args);
                        }
                    } else {
                        ui.label("Installation activated. Reopen agentdocker to use it.");
                    }
                }
            }
            Some(())
        });
        if let Some(error) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        }
        if self.busy {
            ui.label("Verifying desktop installation…");
        }
        if command.is_some() {
            self.busy = true;
            self.error = None;
        }
        command
    }
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
