//! The AgentDocker desktop app: a native window, not a web page. It talks
//! to `agentd` over the same Unix socket as the CLI — one background
//! thread for requests, one for the event stream — and nothing listens on
//! HTTP. Screens: agents by project, the runtimes on this machine, the
//! journal, leases, and events.

mod app;
mod client;
mod desktop;
mod smoke;
mod terminal;

fn main() -> eframe::Result {
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
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("agentdocker")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };
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
