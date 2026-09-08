//! Explicit graphical acceptance mode, used only with fixture IPC paths.
use agentdocker_core::DiscoveredProcess;
use eframe::icon_data::IconDataExt;
use serde_json::json;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicU64, Ordering},
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

// eframe drains capture commands before acquiring a surface. A failed
// acquisition drops that capture, so a later successful paint cannot answer it.
// Recover only after a reported acquisition failure, never on a timer.
const MAX_CAPTURE_ATTEMPTS: u8 = 4;

#[derive(Default)]
struct Capture {
    attempts: u8,
    pending_at_failure: Option<u64>,
}

impl Capture {
    fn request_after_surface(&mut self, failures: u64) -> bool {
        if self
            .pending_at_failure
            .is_some_and(|previous| failures > previous)
        {
            self.pending_at_failure = None;
        }
        if self.pending_at_failure.is_some() || self.attempts >= MAX_CAPTURE_ATTEMPTS {
            return false;
        }
        self.attempts += 1;
        self.pending_at_failure = Some(failures);
        true
    }
}

pub struct Smoke {
    output: PathBuf,
    expected_pid: Option<u32>,
    started: Instant,
    deadline: Duration,
    /// When the last progress line went out, so a stalled run leaves a
    /// trail in the log rather than an empty file.
    reported: Instant,
    capture: Capture,
    surface_failures: Arc<AtomicU64>,
    focus_requested: bool,
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
        // is the screenshot the renderer owes us. A timeout preserves this
        // distinction from failed connection, inventory or discovery.
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
                capture: Capture::default(),
                surface_failures: Arc::new(AtomicU64::new(0)),
                focus_requested: false,
                frames: 0,
                outcome: outcome.clone(),
            },
            outcome,
        ))
    }

    pub fn surface_failures(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.surface_failures)
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
            report["screenshot_requested"] = json!(self.capture.attempts > 0);
            report["capture_attempts"] = json!(self.capture.attempts);
            report["surface_failures"] = json!(self.surface_failures.load(Ordering::Relaxed));
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
            && viewport.visible() != Some(false)
            && viewport.occluded != Some(true)
            && viewport.minimized != Some(true)
            && self
                .capture
                .request_after_surface(self.surface_failures.load(Ordering::Relaxed))
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
        }
        if self.outcome.load(Ordering::Relaxed) == 0 && !self.focus_requested {
            // Focus first, then wait for a visible viewport before requesting
            // capture. Both commands in one frame can race surface readiness.
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.focus_requested = true;
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_acquisition_allows_a_new_capture_but_waiting_alone_does_not() {
        let mut capture = Capture::default();
        assert!(capture.request_after_surface(0));
        for _ in 0..1000 {
            assert!(!capture.request_after_surface(0));
        }
        // The renderer reported a failure after the outstanding request.
        assert!(capture.request_after_surface(1));
        assert!(!capture.request_after_surface(1));
        assert_eq!(capture.attempts, 2);
    }

    #[test]
    fn repeated_surface_failure_does_not_create_unbounded_captures() {
        let mut capture = Capture::default();
        for failure in 0..u64::from(MAX_CAPTURE_ATTEMPTS) {
            assert!(capture.request_after_surface(failure));
        }
        for failure in u64::from(MAX_CAPTURE_ATTEMPTS)..1000 {
            assert!(!capture.request_after_surface(failure));
        }
        assert_eq!(capture.attempts, MAX_CAPTURE_ATTEMPTS);
    }
}
