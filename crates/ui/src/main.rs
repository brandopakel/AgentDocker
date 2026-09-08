//! The AgentDocker desktop app: a native window, not a web page. It talks
//! to `agentd` over the same Unix socket as the CLI — one background
//! thread for requests, one for the event stream — and nothing listens on
//! HTTP. Screens: agents by project, questions put to the person at the
//! keyboard, an agent's terminal, a console for any CLI command, the
//! runtimes on this machine, the journal, leases, and appearance.

mod app;
mod client;
mod desktop;
mod notify;
mod projects;
mod smoke;
mod terminal;
mod theme;

fn main() -> eframe::Result {
    let mut args = std::env::args_os().skip(1);
    let mut smoke_output = None;
    let mut expected_pid = None;
    let mut smoke_deadline = None;
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
                     [--smoke-test OUTPUT --expect-pid PID --smoke-deadline SECONDS]"
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
    if smoke_output.is_none() && (expected_pid.is_some() || smoke_deadline.is_some()) {
        usage_error("--expect-pid and --smoke-deadline require --smoke-test");
    }
    let (smoke, outcome) = match smoke_output {
        Some(output) => match smoke::Smoke::new(output, expected_pid, smoke_deadline) {
            Ok((smoke, outcome)) => (Some(smoke), Some(outcome)),
            Err(error) => {
                eprintln!("{error:#}");
                std::process::exit(2);
            }
        },
        None => (None, None),
    };
    // Our mark, not egui's. eframe falls back to its own logo when the
    // viewport has no icon and then calls `setApplicationIconImage` with
    // it — which overrides the bundle's icon on the *running* Dock tile
    // while Finder still shows ours. That is why the app looked right
    // until you opened it.
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("icon.png"))
        .expect("the bundled icon is a valid PNG");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(icon)
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
    let result = eframe::run_native(
        "AgentDocker",
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

#[cfg(test)]
mod tests {
    /// The window must carry its own icon, and this is why.
    ///
    /// eframe falls back to *its* logo when the viewport has no icon,
    /// and then calls `setApplicationIconImage` with it. macOS shows the
    /// bundle's icon for an app that is not running and the process's
    /// icon for one that is, so the app looked right in Finder and wore
    /// egui's hexagon in the Dock the moment it opened. Nothing about
    /// the bundle could have fixed that.
    #[test]
    fn the_window_carries_our_own_icon() {
        let icon = eframe::icon_data::from_png_bytes(include_bytes!("icon.png"))
            .expect("the embedded icon is a valid PNG");
        assert_eq!(icon.width, 256, "big enough for a retina Dock tile");
        assert_eq!(icon.height, 256);
        assert_ne!(
            icon,
            egui::IconData::default(),
            "an empty icon is the same as not setting one, and eframe \
             would fall back to its own"
        );

        // And it is our mark rather than something else that happens to
        // be 256 square: the tile is dark and the cube is blue, so the
        // blue channel leads by a wide margin over the whole image.
        let (mut red, mut blue) = (0u64, 0u64);
        for pixel in icon.rgba.as_chunks::<4>().0 {
            red += u64::from(pixel[0]);
            blue += u64::from(pixel[2]);
        }
        assert!(blue > red * 2, "the mark is blue: {blue} against {red}");
    }
}
