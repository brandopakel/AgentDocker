//! Bounded fixture-only scenarios exercise the rendered controls' own callbacks.
use crate::{accessibility::Snapshot, app::Message};
use iced::{Task, window};
use serde::Deserialize;
use std::{
    collections::VecDeque,
    path::Path,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    NativeAccessibility,
    Click { id: String },
    Fill { id: String, text: String },
    WaitText { text: String },
    WaitControl { id: String, present: bool },
    Focus { id: String },
    WaitFocus { id: String },
    Capture { name: String },
    Resize { width: u32, height: u32 },
    Pause { millis: u64 },
}
#[derive(Clone)]
pub struct Scenario {
    steps: VecDeque<Step>,
    pub completed: usize,
    pub total: usize,
    pub snapshot: Snapshot,
    pub capture: Option<String>,
    after: Instant,
}
impl Scenario {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(128 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= 128 * 1024, "Scenario exceeds 128 KiB");
        let steps: VecDeque<Step> = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            !steps.is_empty() && steps.len() <= 256,
            "Scenario must have 1–256 steps"
        );
        for step in &steps {
            match step {
                Step::Capture { name } => anyhow::ensure!(
                    !name.is_empty()
                        && name.len() <= 80
                        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                    "Capture name must contain only letters, numbers and hyphens"
                ),
                Step::Resize { width, height } => anyhow::ensure!(
                    (720..=2560).contains(width) && (540..=1600).contains(height),
                    "Invalid capture dimensions"
                ),
                Step::Pause { millis } => {
                    anyhow::ensure!(*millis <= 5000, "Pause exceeds five seconds")
                }
                _ => {}
            }
        }
        Ok(Self {
            total: steps.len(),
            steps,
            completed: 0,
            snapshot: Snapshot::default(),
            capture: None,
            after: Instant::now(),
        })
    }
    pub fn done(&self) -> bool {
        self.steps.is_empty() && self.capture.is_none()
    }
    pub fn waiting(&self) -> String {
        format!("{:?}", self.steps.front())
    }
    pub fn tick(&mut self) -> Task<Message> {
        if self.capture.is_some() || Instant::now() < self.after {
            return Task::none();
        }
        let Some(step) = self.steps.front() else {
            return Task::none();
        };
        let control = |id: &str| self.snapshot.controls.values().find(|c| c.id == id);
        let task = match step {
            Step::NativeAccessibility => window::oldest().and_then(super::native_accessibility),
            Step::Click { id } => {
                let Some(message) = control(id).and_then(|c| c.action.clone()) else {
                    return Task::none();
                };
                Task::done(message)
            }
            Step::Fill { id, text } => {
                let Some(change) = control(id).and_then(|c| c.change.as_ref()) else {
                    return Task::none();
                };
                Task::done(change(text.clone()))
            }
            Step::WaitText { text } => {
                if !self.snapshot.nodes.iter().any(|(_, n)| {
                    n.value().is_some_and(|v| v.contains(text))
                        || n.label().is_some_and(|v| v.contains(text))
                }) {
                    return Task::none();
                }
                Task::none()
            }
            Step::WaitControl { id, present } => {
                if control(id).is_some() != *present {
                    return Task::none();
                }
                Task::none()
            }
            Step::Focus { id } => {
                if control(id).is_none() {
                    return Task::none();
                }
                Task::done(Message::Focus(id.clone()))
            }
            Step::WaitFocus { id } => {
                if self
                    .snapshot
                    .focus
                    .and_then(|node| self.snapshot.controls.get(&node))
                    .is_none_or(|c| c.id != *id)
                {
                    return Task::none();
                }
                Task::none()
            }
            Step::Capture { name } => {
                self.capture = Some(name.clone());
                window::oldest()
                    .and_then(window::screenshot)
                    .map(Message::Captured)
            }
            Step::Resize { width, height } => {
                let size = iced::Size::new(*width as f32, *height as f32);
                window::oldest().and_then(move |id| window::resize(id, size))
            }
            Step::Pause { millis } => {
                self.after = Instant::now() + Duration::from_millis(*millis);
                Task::none()
            }
        };
        if !matches!(step, Step::Pause { .. }) {
            self.after = Instant::now() + Duration::from_millis(180);
        }
        self.steps.pop_front();
        self.completed += 1;
        task
    }
}
