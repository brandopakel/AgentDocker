//! The daemon's own configuration file, `agentd.toml` in its home, read in
//! one place for everything the daemon takes from it: retention windows,
//! webhook sinks and the experimental switches.
use agentdocker_core::config::{DaemonConfig, ExperimentalConfig, FILE_NAME};
use agentdocker_host::policy_file::{self, ReadPolicy};
use std::path::Path;

/// The file as it stands: the defaults when there is none, its word when
/// it reads, and otherwise the reason, for the caller to say once. A TOML
/// error names the bytes, never the text: the file may hold what a sink
/// is called and where its secret lives.
pub(crate) fn read(home: &Path) -> Result<DaemonConfig, String> {
    let path = home.join(FILE_NAME);
    match policy_file::read_changed(&path, None) {
        Ok(ReadPolicy::Absent | ReadPolicy::Unchanged) => Ok(DaemonConfig::default()),
        Ok(ReadPolicy::Text { text, .. }) => toml::from_str::<DaemonConfig>(&text)
            .map_err(|error| format!("{}: {}", path.display(), redacted(&error))),
        Err(error) => Err(format!("cannot read {}: {}", path.display(), error.kind())),
    }
}

/// The experimental switches as this daemon applies them: the file's,
/// with this process's environment on top. A file that does not read
/// leaves the defaults; the retention tick says why, once.
pub(crate) fn experimental(home: &Path) -> ExperimentalConfig {
    read(home)
        .map(|config| config.experimental)
        .unwrap_or_default()
        .with_env(|name| std::env::var(name).ok())
}

fn redacted(error: &toml::de::Error) -> String {
    match error.span() {
        Some(span) => format!(
            "{FILE_NAME} is not valid TOML at bytes {}..{}",
            span.start, span.end
        ),
        None => format!("{FILE_NAME} is not valid TOML"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_is_read_once_for_everything_and_its_text_stays_out_of_the_notice() {
        let home = tempfile::tempdir().unwrap();
        let absent = read(home.path()).unwrap();
        assert_eq!(absent, DaemonConfig::default());
        assert!(experimental(home.path()).native_codex);
        std::fs::write(
            home.path().join(FILE_NAME),
            "[agents]\nretention = \"7d\"\n[experimental]\nreload = true\nsecret_input = true\n",
        )
        .unwrap();
        let config = read(home.path()).unwrap();
        assert_eq!(config.agents.retention, "7d");
        assert!(config.experimental.reload && config.experimental.secret_input);
        assert!(experimental(home.path()).reload);
        std::fs::write(
            home.path().join(FILE_NAME),
            "[[webhooks]]\nname = \"team\"\nsecret_file = \"/private/hook.secret\"\nurl = https://hooks.example/x\n",
        )
        .unwrap();
        let notice = read(home.path()).unwrap_err();
        assert!(notice.contains("is not valid TOML at bytes"), "{notice}");
        assert!(
            !notice.contains("hook.secret") && !notice.contains("hooks.example"),
            "{notice}"
        );
        assert_eq!(
            experimental(home.path()).reload,
            std::env::var(ExperimentalConfig::RELOAD_ENV).as_deref() == Ok("1"),
            "an unreadable file leaves the defaults, with the environment's word on top"
        );
    }
}
