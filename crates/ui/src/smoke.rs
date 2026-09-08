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

/// How long the window will wait to become ready before giving up and
/// saying what it was still waiting for.
///
/// Generous on purpose. This is the *only* budget that matters: the
/// harness around it waits longer, so a run that fails fails here,
/// where the unmet condition is known, rather than there, where all
/// that is known is that the window never exited. Two deadlines close
/// together is how a graphical check becomes flaky with no evidence.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(60);

// Asked for exactly once, and this is not an oversight. Sending
// `ViewportCommand::Screenshot` again before the first has been
// answered replaces the pending request rather than queuing a second
// one, so a retry loop asks forever and is answered never: measured at
// 12 failures in 12 runs with a two-second retry against 15 passes in
// 15 without one. If the single reply is ever genuinely lost, the
// deadline below reports it by name rather than hanging silently.

pub struct Smoke {
    output: PathBuf,
    expected_pid: Option<u32>,
    started: Instant,
    deadline: Duration,
    /// When the last progress line went out, so a stalled run leaves a
    /// trail in the log rather than an empty file.
    reported: Instant,
    /// When a screenshot was last asked for, or `None` before the first
    /// ask. See `RETRY_AFTER`.
    requested: Option<Instant>,
    frames: usize,
    outcome: Arc<AtomicU8>,
}

/// What is still missing, in the words of the conditions themselves.
fn unmet(connected: bool, runtimes: usize, fixture: bool, frames: usize) -> String {
    let mut waiting = Vec::new();
    if !connected {
        waiting.push("a daemon connection".to_owned());
    }
    if runtimes == 0 {
        waiting.push("the runtime inventory".to_owned());
    }
    if !fixture {
        waiting.push("the fixture process to be discovered".to_owned());
    }
    if frames < 3 {
        waiting.push(format!("frames to render ({frames} so far)"));
    }
    if waiting.is_empty() {
        // Everything the window waits for has happened, so what is left
        // is the screenshot the renderer owes us. It is asked for again
        // every RETRY_AFTER, so reaching the deadline here means every
        // one of those went unanswered.
        return "the renderer to hand back a screenshot".to_owned();
    }
    waiting.join(", ")
}

impl Smoke {
    pub fn new(
        output: PathBuf,
        expected_pid: Option<u32>,
        deadline: Option<Duration>,
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
                deadline: deadline.unwrap_or(DEFAULT_DEADLINE),
                reported: Instant::now(),
                requested: None,
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
        } else if self.started.elapsed() > self.deadline {
            // Named, not merely reported as "not ready": a run that
            // fails on a machine nobody can attach to is only as useful
            // as what it wrote down.
            Some(Err(anyhow::anyhow!(
                "gave up after {}s waiting for: {}",
                self.deadline.as_secs(),
                unmet(connected, runtimes, fixture, self.frames)
            )))
        } else {
            None
        };
        if completed.is_none() && self.reported.elapsed() >= Duration::from_secs(1) {
            self.reported = Instant::now();
            eprintln!(
                "{:.0}s waiting for: {}",
                self.started.elapsed().as_secs_f64(),
                unmet(connected, runtimes, fixture, self.frames)
            );
        }
        if let Some(result) = completed {
            let success = result.is_ok() && connected && runtimes > 0 && fixture;
            let mut report = result
                .unwrap_or_else(|error| json!({"result":"failed", "error":error.to_string()}));
            // Preserve the unmet condition when CI cannot reach capture. Counts
            // and fixture readiness carry no discovered commands or user paths.
            report["connected"] = json!(connected);
            report["runtime_rows"] = json!(runtimes);
            report["fixture_discovered"] = json!(fixture);
            report["screenshot_requested"] = json!(self.requested.is_some());
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
        } else if connected
            && runtimes > 0
            && fixture
            && self.frames >= 3
            && self.requested.is_none()
        {
            eprintln!(
                "graphical acceptance screenshot requested at {:?}: visible={:?}, occluded={:?}, minimized={:?}, focused={:?}",
                self.started.elapsed(),
                viewport.visible(),
                viewport.occluded,
                viewport.minimized,
                viewport.focused
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            self.requested = Some(Instant::now());
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}
