//! Explicit graphical acceptance mode, used only with fixture IPC paths.
use agentdocker_core::DiscoveredProcess;
use eframe::icon_data::IconDataExt;
use serde_json::json;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::time::{Duration, Instant};

pub struct Smoke {
    output: PathBuf,
    expected_pid: Option<u32>,
    started: Instant,
    requested: bool,
    frames: usize,
    outcome: Arc<AtomicU8>,
}

impl Smoke {
    pub fn new(
        output: PathBuf,
        expected_pid: Option<u32>,
    ) -> anyhow::Result<(Self, Arc<AtomicU8>)> {
        anyhow::ensure!(
            std::env::var_os("AGENTDOCKER_HOME").is_some()
                && std::env::var_os("AGENTDOCKER_SOCKET").is_some(),
            "graphical acceptance requires explicit fixture home and socket"
        );
        anyhow::ensure!(
            !output.exists(),
            "smoke output already exists; use a fresh directory"
        );
        agentdocker_host::dirs::secure_state_dir(&output)?;
        let outcome = Arc::new(AtomicU8::new(0));
        Ok((
            Self {
                output,
                expected_pid,
                started: Instant::now(),
                requested: false,
                frames: 0,
                outcome: outcome.clone(),
            },
            outcome,
        ))
    }

    pub fn tick(
        &mut self,
        ctx: &egui::Context,
        connected: bool,
        runtimes: usize,
        discovered: &[DiscoveredProcess],
    ) {
        if self.outcome.load(Ordering::Relaxed) != 0 {
            return;
        }
        self.frames += 1;
        let fixture = self
            .expected_pid
            .is_none_or(|pid| discovered.iter().any(|agent| agent.pid == pid));
        let viewport = ctx.input(|input| input.viewport().clone());
        let screenshot = ctx.input(|input| {
            input.events.iter().find_map(|event| {
                if let egui::Event::Screenshot { image, .. } = event {
                    Some(image.clone())
                } else {
                    None
                }
            })
        });
        let completed = if let Some(image) = screenshot {
            let icon = egui::IconData {
                width: image.size[0] as u32,
                height: image.size[1] as u32,
                rgba: image
                    .pixels
                    .iter()
                    .flat_map(|pixel| pixel.to_array())
                    .collect(),
            };
            let saved = icon
                .to_png_bytes()
                .map_err(anyhow::Error::msg)
                .and_then(|png| {
                    use std::io::Write;
                    agentdocker_host::dirs::private_file(
                        &self.output.join("window.png"),
                        true,
                        false,
                    )?
                    .write_all(&png)?;
                    Ok(())
                });
            Some(saved.and_then(|()| {
                anyhow::ensure!(
                    connected && runtimes > 0 && fixture,
                    "connection or fixture was lost before capture"
                );
                Ok(json!({"result":"passed", "display_name":"agentdocker",
                "connected":connected, "runtime_rows":runtimes, "fixture_discovered":fixture,
                "frames":self.frames, "screenshot":"window.png"}))
            }))
        } else if self.started.elapsed() > Duration::from_secs(30) {
            Some(Err(anyhow::anyhow!(
                "native window did not reach readiness and complete a screenshot"
            )))
        } else {
            None
        };
        if let Some(result) = completed {
            let success = result.is_ok() && connected && runtimes > 0 && fixture;
            let mut report = result
                .unwrap_or_else(|error| json!({"result":"failed", "error":error.to_string()}));
            // Preserve the unmet condition when CI cannot reach capture. Counts
            // and fixture readiness carry no discovered commands or user paths.
            report["connected"] = json!(connected);
            report["runtime_rows"] = json!(runtimes);
            report["fixture_discovered"] = json!(fixture);
            report["screenshot_requested"] = json!(self.requested);
            report["frames"] = json!(self.frames);
            report["elapsed_seconds"] = json!(self.started.elapsed().as_secs_f64());
            report["viewport_visible"] = json!(viewport.visible());
            report["viewport_occluded"] = json!(viewport.occluded);
            report["viewport_minimized"] = json!(viewport.minimized);
            report["viewport_focused"] = json!(viewport.focused);
            let write = (|| -> anyhow::Result<()> {
                use std::io::Write;
                agentdocker_host::dirs::private_file(
                    &self.output.join("result.json"),
                    true,
                    false,
                )?
                .write_all(serde_json::to_string_pretty(&report)?.as_bytes())?;
                Ok(())
            })();
            self.outcome.store(
                if success && write.is_ok() { 1 } else { 2 },
                Ordering::Relaxed,
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if connected && runtimes > 0 && fixture && self.frames >= 3 && !self.requested {
            eprintln!(
                "graphical acceptance screenshot requested at {:?}: visible={:?}, occluded={:?}, minimized={:?}, focused={:?}",
                self.started.elapsed(),
                viewport.visible(),
                viewport.occluded,
                viewport.minimized,
                viewport.focused
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            self.requested = true;
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}
