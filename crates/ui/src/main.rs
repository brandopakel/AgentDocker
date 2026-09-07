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
            // Small enough to be honest about: below this the agent
            // table's columns start colliding, and a window that cannot
            // show its own contents is worse than one that scrolls.
            .with_min_inner_size([680.0, 460.0])
            // The preferred size is a preference, not a demand. A 1100
            // by 720 window does not fit a 1280 by 800 laptop once the
            // menu bar and the Dock have taken their share, and a window
            // that opens larger than the screen opens with its own
            // controls off the edge. Off by default everywhere but
            // Linux, so it has to be asked for.
            .with_clamp_size_to_monitor_size(true),
        ..Default::default()
    };
    eframe::run_native(
        "AgentDocker",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
