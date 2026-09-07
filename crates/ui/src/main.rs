//! The AgentDocker desktop app: a native window, not a web page. It talks
//! to `agentd` over the same Unix socket as the CLI — one background
//! thread for requests, one for the event stream — and nothing listens on
//! HTTP. Screens: agents by project, questions put to the person at the
//! keyboard, an agent's terminal, a console for any CLI command, the
//! runtimes on this machine, the journal, leases, and appearance.

mod app;
mod client;
mod desktop;
mod projects;
mod smoke;
mod terminal;
mod theme;

fn main() -> eframe::Result {
    let _installation_pin = agentdocker_host::installation::pin_current_executable()
        .unwrap_or_else(|error| usage_error(&format!("cannot open installed release: {error}")));
    let mut args = std::env::args_os().skip(1);
    let mut smoke_output = None;
    let mut expected_pid = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--version" | "-V") => {
                println!("agentdocker-ui {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            Some("--smoke-test") => {
                smoke_output = Some(
                    args.next()
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| {
                            usage_error("--smoke-test requires an output directory")
                        }),
                );
            }
            Some("--expect-pid") => {
                expected_pid = Some(
                    args.next()
                        .and_then(|pid| pid.to_str()?.parse::<u32>().ok())
                        .filter(|pid| *pid > 0)
                        .unwrap_or_else(|| {
                            usage_error("--expect-pid requires a positive process id")
                        }),
                );
            }
            Some("--help" | "-h") => {
                println!("agentdocker-ui [--version] [--smoke-test OUTPUT --expect-pid PID]");
                return Ok(());
            }
            Some(value) if cfg!(target_os = "macos") && value.starts_with("-psn_") => (),
            _ => {
                eprintln!("unknown argument: {}", arg.to_string_lossy());
                std::process::exit(2);
            }
        }
    }
    if expected_pid.is_some() && smoke_output.is_none() {
        usage_error("--expect-pid requires --smoke-test");
    }
    let (smoke, outcome) = match smoke_output {
        Some(output) => match smoke::Smoke::new(output, expected_pid) {
            Ok((smoke, outcome)) => (Some(smoke), Some(outcome)),
            Err(error) => {
                eprintln!("{error:#}");
                std::process::exit(2);
            }
        },
        None => (None, None),
    };
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("agentdocker")
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
    if smoke.is_some() {
        // Keep renderer failures observable in explicit fixture mode. Preserve
        // the backend's recovery behavior and bound repetitive log messages.
        let original = options.wgpu_options.on_surface_status.clone();
        let count = std::sync::atomic::AtomicU64::new(0);
        options.wgpu_options.on_surface_status = std::sync::Arc::new(move |status| {
            let count = count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if count <= 16 || count.is_power_of_two() {
                eprintln!("graphical acceptance surface #{count}: {status:?}");
            }
            original(status)
        });
    }
    let result = eframe::run_native(
        "agentdocker",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc).with_smoke(smoke)))),
    );
    if outcome.is_some_and(|state| state.load(std::sync::atomic::Ordering::Relaxed) != 1) {
        std::process::exit(1);
    }
    result
}

fn usage_error(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}
