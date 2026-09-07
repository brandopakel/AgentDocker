//! The AgentDocker desktop app: a native window, not a web page. It talks
//! to `agentd` over the same Unix socket as the CLI — one background
//! thread for requests, one for the event stream — and nothing listens on
//! HTTP. Screens: agents by project, questions put to the person at the
//! keyboard, an agent's terminal, a console for any CLI command, the
//! runtimes on this machine, the journal, leases, and appearance.

mod app;
mod client;
mod projects;
mod terminal;
mod theme;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("AgentDocker")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "AgentDocker",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
