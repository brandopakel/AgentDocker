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
        if self.local_preview && matches!(operation, "install" | "rollback") {
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
                if ui.button("Preview removal").clicked() {
                    let mut args = self.command("uninstall");
                    args.push("--preview".into());
                    command = Some(args);
                }
                if ui.button("Preview cleanup").clicked() {
                    let mut args = self.command("prune");
                    args.push("--preview".into());
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
                if let Some(plan) = report.get("maintenance") {
                    ui.label("Running sessions and your settings are preserved. Installed user services must be removed separately before removing desktop launchers or pruning versions.");
                    if let Some(paths) = plan["remove"].as_array() {
                        for path in paths { ui.label(format!("Remove: {}", path.as_str().unwrap_or("unknown"))); }
                    }
                    if let Some(entries) = plan["retained"].as_array() {
                        for entry in entries { ui.label(format!("Keep: {} — {}", entry["path"].as_str().unwrap_or("unknown"), entry["reason"].as_str().unwrap_or("unknown"))); }
                    }
                    if report["preview"] == true && plan["remove"].as_array().is_some_and(|paths| !paths.is_empty()) {
                        if ui.button("Apply reviewed cleanup").clicked() {
                            let mut args = self.command(plan["operation"].as_str()?);
                            args.extend(["--expect-plan".into(), report["plan_id"].as_str()?.into()]);
                            if let Some(keep) = plan["keep"].as_u64() { args.extend(["--keep".into(), keep.to_string()]); }
                            command = Some(args);
                        }
                    } else if report["preview"] == false {
                        ui.label("Cleanup completed.");
                    }
                } else if report.get("installation").is_some() {
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
