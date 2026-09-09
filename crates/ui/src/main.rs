//! The AgentDocker desktop app: a native window, not a web page. It talks
//! to `agentd` over the same Unix socket as the CLI — one background
//! thread for requests, one for the event stream — and nothing listens on
//! HTTP. Screens: agents by project, questions put to the person at the
//! keyboard, an agent's terminal, a console for any CLI command, the
//! runtimes on this machine, the journal, leases, and appearance.

mod accessibility;
mod app;
mod catalog;
mod client;
mod color;
mod controls;
mod desktop;
mod notify;
mod smoke;
mod terminal;
mod theme;
mod wake;

/// What the window says about itself, and where.
///
/// Iced, winit and the native adapters all report through the `log` crate, and until
/// this was here nothing collected them: a renderer that refused to hand
/// back a frame, a surface that could not be created, a device lost —
/// every one of those was discarded, and a graphical failure left
/// nothing behind but the fact that it had failed. `tracing-subscriber`
/// bridges `log`, so one subscriber catches both.
///
/// Warnings and errors by default, because a window is not a daemon and
/// its stderr is usually a terminal somebody is reading; `RUST_LOG` for
/// when more is wanted.
fn logging() {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();
}

fn main() -> iced::Result {
    logging();
    let _installation_pin = agentdocker_host::installation::pin_current_executable()
        .unwrap_or_else(|error| usage_error(&format!("cannot open installed release: {error}")));
    let mut args = std::env::args_os().skip(1);
    let mut smoke_output = None;
    let mut expected_pid = None;
    let mut smoke_deadline = None;
    let mut smoke_scenario = None;
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
            Some("--smoke-scenario") => {
                smoke_scenario = Some(
                    args.next()
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| usage_error("--smoke-scenario requires a JSON file")),
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
            // One notification, then exit. The daemon runs this from
            // inside the app bundle so the notification carries our
            // icon; nothing else on macOS can.
            Some("--notify") => {
                let title = args
                    .next()
                    .unwrap_or_else(|| usage_error("--notify requires a title and a body"));
                let body = args
                    .next()
                    .unwrap_or_else(|| usage_error("--notify requires a title and a body"));
                let said = |value: std::ffi::OsString| value.to_string_lossy().into_owned();
                return match notify::post(&said(title), &said(body)) {
                    Ok(()) => Ok(()),
                    Err(reason) => {
                        eprintln!("{reason}");
                        std::process::exit(1);
                    }
                };
            }
            Some("--smoke-deadline") => {
                smoke_deadline = Some(
                    args.next()
                        .and_then(|secs| secs.to_str()?.parse::<u64>().ok())
                        .filter(|secs| (1..=600).contains(secs))
                        .map(std::time::Duration::from_secs)
                        .unwrap_or_else(|| usage_error("--smoke-deadline requires 1-600 seconds")),
                );
            }
            Some("--help" | "-h") => {
                println!(
                    "agentdocker-ui [--version] [--notify TITLE BODY] \
                     [--smoke-test OUTPUT --expect-pid PID --smoke-deadline SECONDS --smoke-scenario JSON]"
                );
                return Ok(());
            }
            Some(value) if cfg!(target_os = "macos") && value.starts_with("-psn_") => (),
            _ => {
                eprintln!("unknown argument: {}", arg.to_string_lossy());
                std::process::exit(2);
            }
        }
    }
    if smoke_output.is_none()
        && (expected_pid.is_some() || smoke_deadline.is_some() || smoke_scenario.is_some())
    {
        usage_error("Smoke options require --smoke-test");
    }
    let (smoke, outcome) = match smoke_output {
        Some(output) => {
            match smoke::Smoke::new(output, expected_pid, smoke_deadline, smoke_scenario) {
                Ok((smoke, outcome)) => (Some(smoke), Some(outcome)),
                Err(error) => {
                    eprintln!("{error:#}");
                    std::process::exit(2);
                }
            }
        }
        None => (None, None),
    };
    let mut reader = png::Decoder::new(std::io::Cursor::new(include_bytes!("icon.png")))
        .read_info()
        .expect("embedded icon");
    let mut rgba = vec![0; reader.output_buffer_size().expect("icon buffer")];
    let info = reader.next_frame(&mut rgba).expect("embedded PNG");
    rgba.truncate(info.buffer_size());
    let icon = iced::window::icon::from_rgba(rgba, info.width, info.height).expect("RGBA icon");
    let result = iced::application(
        move || {
            let (app, task) = app::App::boot();
            (app.with_smoke(smoke.clone()), task)
        },
        app::App::update,
        app::App::view,
    )
    .title("agentdocker")
    .theme(app::App::theme)
    .scale_factor(app::App::scale_factor)
    .subscription(app::App::subscription)
    .window(iced::window::Settings {
        size: iced::Size::new(1180.0, 760.0),
        min_size: Some(iced::Size::new(720.0, 540.0)),
        visible: false,
        exit_on_close_request: false,
        icon: Some(icon),
        ..Default::default()
    })
    .centered()
    .run();
    if outcome.is_some_and(|state| state.load(std::sync::atomic::Ordering::Relaxed) != 1) {
        std::process::exit(1);
    }
    result
}

fn usage_error(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_window_carries_our_own_icon() {
        let mut reader = png::Decoder::new(std::io::Cursor::new(include_bytes!("icon.png")))
            .read_info()
            .unwrap();
        let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut bytes).unwrap();
        assert_eq!((info.width, info.height), (256, 256));
        let (mut red, mut blue) = (0u64, 0u64);
        for pixel in bytes[..info.buffer_size()].as_chunks::<4>().0 {
            red += u64::from(pixel[0]);
            blue += u64::from(pixel[2]);
        }
        assert!(
            blue > red * 2,
            "The window carries the blue AgentDocker mark"
        );
    }
}
