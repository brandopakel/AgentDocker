//! An isolated, interactive design preview with explicitly fictional data.
mod model;
mod style;
mod view;

use iced::{Size, Subscription, Task, Theme, keyboard, window};
use model::{Detail, Page, Workspace};
use std::{
    io::{BufWriter, Cursor},
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
enum Message {
    Navigate(Page),
    Project(usize),
    Select(usize),
    CloseDetail,
    Detail(Detail),
    Search(String),
    Draft(String),
    Answer,
    Dark(bool),
    Scene(bool, bool),
    Connection(usize),
    Key(keyboard::Event),
    Resized(Size),
    Tick,
    Captured(window::Screenshot),
}

struct App {
    workspace: Workspace,
    icon: iced::widget::image::Handle,
    width: f32,
    screenshot: Option<PathBuf>,
    capture_requested: bool,
    started: Instant,
}

impl App {
    fn update(&mut self, message: Message) -> Task<Message> {
        let w = &mut self.workspace;
        match message {
            Message::Navigate(page) => w.page = page,
            Message::Project(project) => w.select_project(project),
            Message::Select(id) => {
                w.selected = Some(id);
                w.detail = Detail::Overview;
            }
            Message::CloseDetail => w.selected = None,
            Message::Detail(detail) => w.detail = detail,
            Message::Search(value) => {
                w.search = value;
                w.selected = None;
            }
            Message::Draft(value) => w.answer = value.chars().take(4000).collect(),
            Message::Answer => w.send_answer(),
            Message::Dark(dark) => w.dark = dark,
            Message::Scene(empty, offline) => {
                w.empty = empty;
                w.offline = offline;
                w.page = Page::Projects;
                w.selected = None;
            }
            Message::Connection(id) => w.connection = (w.connection != Some(id)).then_some(id),
            Message::Resized(size) => self.width = size.width,
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                use keyboard::{Key, key::Named};
                match key {
                    Key::Named(Named::Tab) => {
                        return if modifiers.shift() {
                            iced::widget::operation::focus_previous()
                        } else {
                            iced::widget::operation::focus_next()
                        };
                    }
                    Key::Named(Named::Escape) => w.selected = None,
                    Key::Character(key) if modifiers.command() => {
                        if let Some(page) = match key.as_str() {
                            "1" => Some(Page::Projects),
                            "2" => Some(Page::Inbox),
                            "3" => Some(Page::Connections),
                            "4" => Some(Page::Settings),
                            _ => None,
                        } {
                            w.page = page;
                        }
                    }
                    _ => {}
                }
            }
            Message::Key(_) => {}
            Message::Tick => {
                if self.started.elapsed() > Duration::from_secs(15) {
                    eprintln!("design capture exceeded its deadline");
                    std::process::exit(1);
                }
                if !self.capture_requested && self.started.elapsed() > Duration::from_millis(800) {
                    self.capture_requested = true;
                    return window::oldest()
                        .and_then(window::screenshot)
                        .map(Message::Captured);
                }
            }
            Message::Captured(capture) => {
                if let Some(path) = self.screenshot.take() {
                    if let Err(error) = save_capture(&path, capture) {
                        eprintln!("cannot save design capture: {error}");
                        std::process::exit(1);
                    }
                    println!("Saved {}", path.display());
                    return iced::exit();
                }
            }
        }
        Task::none()
    }

    fn theme(&self) -> Theme {
        style::Colors::new(self.workspace.dark).theme()
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            keyboard::listen().map(Message::Key),
            window::resize_events().map(|(_, size)| Message::Resized(size)),
        ];
        if self.screenshot.is_some() {
            subscriptions
                .push(iced::time::every(Duration::from_millis(200)).map(|_| Message::Tick));
        }
        Subscription::batch(subscriptions)
    }
}

fn save_capture(
    path: &std::path::Path,
    capture: window::Screenshot,
) -> Result<(), Box<dyn std::error::Error>> {
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let mut encoder = png::Encoder::new(
        BufWriter::new(output),
        capture.size.width,
        capture.size.height,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&capture.rgba)?;
    Ok(())
}

fn main() -> iced::Result {
    let mut workspace = Workspace::default();
    let mut screenshot = None;
    let mut width = 1180.0;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--screenshot" => {
                screenshot = Some(PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| usage("missing screenshot path")),
                ))
            }
            "--dark" => workspace.dark = true,
            "--offline" => workspace.offline = true,
            "--empty" => workspace.empty = true,
            "--details" => workspace.selected = Some(0),
            "--width" => {
                width = args
                    .next()
                    .and_then(|s| s.parse::<f32>().ok())
                    .filter(|n| (720.0..=1920.0).contains(n))
                    .unwrap_or_else(|| usage("width must be 720–1920"))
            }
            "--page" => {
                workspace.page = match args.next().as_deref() {
                    Some("projects") => Page::Projects,
                    Some("inbox") => Page::Inbox,
                    Some("connections") => Page::Connections,
                    Some("settings") => Page::Settings,
                    _ => usage("unknown page"),
                }
            }
            "--help" | "-h" => {
                println!(
                    "agentdocker-design [--dark] [--offline] [--empty] [--details] [--page projects|inbox|connections|settings] [--width 720..1920] [--screenshot NEW.png]\nAn interactive design preview. All data is fictional; no daemon or provider is contacted."
                );
                return Ok(());
            }
            _ => usage("unknown argument"),
        }
    }
    let mut reader = png::Decoder::new(Cursor::new(include_bytes!("../../ui/src/icon.png")))
        .read_info()
        .expect("embedded icon");
    let mut bytes = vec![0; reader.output_buffer_size().expect("icon buffer size")];
    let info = reader.next_frame(&mut bytes).expect("valid embedded PNG");
    bytes.truncate(info.buffer_size());
    let icon = iced::widget::image::Handle::from_rgba(info.width, info.height, bytes.clone());
    let window_icon = window::icon::from_rgba(bytes, info.width, info.height).expect("RGBA icon");
    iced::application(
        move || App {
            workspace: workspace.clone(),
            icon: icon.clone(),
            width,
            screenshot: screenshot.clone(),
            capture_requested: false,
            started: Instant::now(),
        },
        App::update,
        App::view,
    )
    .title("agentdocker · Design preview")
    .theme(App::theme)
    .subscription(App::subscription)
    .window(window::Settings {
        size: Size::new(width, 760.0),
        min_size: Some(Size::new(720.0, 540.0)),
        icon: Some(window_icon),
        ..Default::default()
    })
    .centered()
    .run()
}

fn usage(message: &str) -> ! {
    eprintln!("{message}; use --help");
    std::process::exit(2)
}
