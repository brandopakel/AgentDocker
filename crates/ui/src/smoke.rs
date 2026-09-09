//! Explicit native capture acceptance, restricted to isolated fixture state.
use crate::app::Message;
use agentdocker_core::DiscoveredProcess;
use iced::{Task, window};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};
mod scenario;
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(60);
#[derive(Clone)]
pub struct Smoke {
    output: PathBuf,
    expected_pid: Option<u32>,
    started: Instant,
    deadline: Duration,
    requested: bool,
    ticks: usize,
    ready: bool,
    runtimes: usize,
    outcome: Arc<AtomicU8>,
    pub scenario: Option<scenario::Scenario>,
    native_nodes: Option<usize>,
}
impl Smoke {
    pub fn new(
        output: PathBuf,
        expected_pid: Option<u32>,
        deadline: Option<Duration>,
        scenario: Option<PathBuf>,
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
                requested: false,
                ticks: 0,
                ready: false,
                runtimes: 0,
                outcome: outcome.clone(),
                native_nodes: None,
                scenario: scenario.map(|p| scenario::Scenario::load(&p)).transpose()?,
            },
            outcome,
        ))
    }
    pub fn tick(
        &mut self,
        connected: bool,
        runtimes: usize,
        discovered: &[DiscoveredProcess],
    ) -> Task<Message> {
        self.ticks += 1;
        self.runtimes = runtimes;
        let fixture = self
            .expected_pid
            .is_none_or(|pid| discovered.iter().any(|p| p.pid == pid));
        self.ready = connected && runtimes > 0 && fixture;
        if self.started.elapsed() > self.deadline {
            self.finish(Err(format!("Timed out: scenario={},  connected={connected}, runtime_rows={runtimes}, fixture_discovered={fixture}, capture_requested={}",self.scenario.as_ref().map_or_else(||"none".into(), |s|s.waiting()), self.requested)));
            return iced::exit();
        }
        if self.ready
            && self.started.elapsed() > Duration::from_millis(500)
            && let Some(scenario) = &mut self.scenario
            && !scenario.done()
        {
            let task = scenario.tick();
            let progress = serde_json::json!({"completed":scenario.completed,"waiting":scenario.waiting(),"controls":scenario.snapshot.controls.values().map(|c| (&c.id, c.action.is_some(), c.change.is_some())).collect::<Vec<_>>()});
            use std::io::Write;
            if let Ok(mut file) = agentdocker_host::dirs::private_file(
                &self.output.join("progress.json"),
                true,
                false,
            ) {
                let _ = file.set_len(0);
                let _ = file.write_all(progress.to_string().as_bytes());
            }
            return task;
        }
        if self.ready
            && self.ticks >= 3
            && self.started.elapsed() > Duration::from_millis(500)
            && !self.requested
        {
            self.requested = true;
            return window::oldest()
                .and_then(window::screenshot)
                .map(Message::Captured);
        }
        Task::none()
    }
    pub fn captured(&mut self, capture: window::Screenshot) -> Task<Message> {
        let result = (|| -> anyhow::Result<()> {
            anyhow::ensure!(self.ready, "Connection readiness was lost before capture");
            anyhow::ensure!(
                capture.size.width >= 640 && capture.size.height >= 400,
                "Native capture is unexpectedly small"
            );
            let name = self
                .scenario
                .as_mut()
                .and_then(|s| s.capture.take())
                .unwrap_or_else(|| "window".into());
            let file = agentdocker_host::dirs::private_file(
                &self.output.join(format!("{name}.png")),
                true,
                false,
            )?;
            let mut encoder = png::Encoder::new(
                std::io::BufWriter::new(file),
                capture.size.width,
                capture.size.height,
            );
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header()?.write_image_data(&capture.rgba)?;
            Ok(())
        })();
        if result.is_ok() && !self.requested {
            return Task::none();
        }
        self.finish(result.map_err(|e| e.to_string()));
        Task::done(Message::Event(iced::Event::Window(
            window::Event::CloseRequested,
        )))
    }
    pub fn native_accessibility(&mut self, result: Result<usize, String>) -> Task<Message> {
        match result {
            Ok(count) => {
                self.native_nodes = Some(count);
                Task::none()
            }
            Err(error) => {
                self.finish(Err(error));
                iced::exit()
            }
        }
    }
    fn finish(&mut self, result: Result<(), String>) {
        use std::io::Write;
        let passed = result.is_ok();
        let report = serde_json::json!({"result":if passed{"passed"}else{"failed"},"error":result.err(),"renderer":"iced tiny-skia","native_accessibility_nodes":self.native_nodes,"connected":self.ready,"runtime_rows":self.runtimes,"fixture_discovered":self.ready,"screenshot_requested":self.requested,"capture_attempts":usize::from(self.requested),"scenario_steps_completed":self.scenario.as_ref().map(|s|s.completed),"scenario_steps_total":self.scenario.as_ref().map(|s|s.total),"ticks":self.ticks,"elapsed_seconds":self.started.elapsed().as_secs_f64()});
        let write = (|| -> anyhow::Result<()> {
            agentdocker_host::dirs::private_file(&self.output.join("result.json"), true, false)?
                .write_all(&serde_json::to_vec_pretty(&report)?)?;
            Ok(())
        })();
        self.outcome.store(
            if passed && write.is_ok() { 1 } else { 2 },
            Ordering::Relaxed,
        );
    }
}

fn native_accessibility(id: window::Id) -> Task<Message> {
    window::run(id, |window| {
        #[cfg(target_os = "macos")]
        {
            use objc2::{msg_send, rc::Retained, runtime::AnyObject};
            use objc2_foundation::{NSArray, NSString};
            fn titles(node: &AnyObject, depth: usize, names: &mut Vec<String>) {
                if depth > 4 || names.len() > 512 {
                    return;
                }
                // These objects belong to this app's NSAccessibility hierarchy;
                // the calls run on the window thread while the NSView is retained.
                let title: Option<Retained<NSString>> =
                    unsafe { msg_send![node, accessibilityTitle] };
                if let Some(title) = title {
                    names.push(title.to_string());
                }
                let children: Option<Retained<NSArray<AnyObject>>> =
                    unsafe { msg_send![node, accessibilityChildren] };
                if let Some(children) = children {
                    for child in children.iter().take(512) {
                        titles(&child, depth + 1, names);
                    }
                }
            }
            let handle = window.window_handle().map_err(|e| e.to_string())?;
            let raw_window_handle::RawWindowHandle::AppKit(handle) = handle.as_raw() else {
                return Err("Expected AppKit view".into());
            };
            let view = unsafe { &*handle.ns_view.as_ptr().cast::<AnyObject>() };
            let mut names = Vec::new();
            titles(view, 0, &mut names);
            if names.iter().any(|n| n == "Projects") && names.iter().any(|n| n == "Settings") {
                Ok(names.len())
            } else {
                Err(format!(
                    "Native NSAccessibility did not expose navigation controls: {names:?}"
                ))
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = window;
            Err("Native accessibility probe currently requires macOS".into())
        }
    })
    .map(Message::NativeAccessibility)
}
