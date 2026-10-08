//! Recognize exact emitted definitions, never evaluate shell or unit syntax.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
pub(super) struct Definition {
    pub(super) argv: Vec<String>,
    pub(super) environment: BTreeMap<String, String>,
    pub(super) log: PathBuf,
    pub(super) unit: String,
    pub(super) plist: String,
    pub(super) legacy_unit: String,
    pub(super) literal_command: bool,
    connector: bool,
}

impl Definition {
    fn new(
        connector: bool,
        argv: Vec<String>,
        environment: BTreeMap<String, String>,
    ) -> Result<Self> {
        ensure!(!argv.is_empty(), "missing service command");
        let (unit, plist, log) = if connector {
            ensure!(
                argv.get(1).map(String::as_str) == Some("connector")
                    && argv.get(2).map(String::as_str) == Some("serve"),
                "unknown connector command"
            );
            crate::connector::service::argument_resources(&argv[3..])?;
            ensure!(environment.len() == 2, "unknown connector environment");
            let home = PathBuf::from(
                environment
                    .get("AGENTDOCKER_HOME")
                    .context("missing connector home")?,
            );
            let path_dirs = environment
                .get("PATH")
                .context("missing connector PATH")?
                .split(':')
                .map(PathBuf::from)
                .collect();
            let layout = crate::connector::service::Layout {
                agentdocker: argv[0].clone().into(),
                home,
                socket: None,
                user_home: PathBuf::from("/"),
                uid: 0,
                serve_args: argv[3..].to_vec(),
                path_dirs,
            };
            (
                crate::connector::service::systemd_unit(&layout),
                crate::connector::service::launchd_plist(&layout),
                layout.log(),
            )
        } else {
            ensure!(
                (argv.len() == 3 || argv.len() == 5)
                    && argv[1] == "--home"
                    && (argv.len() == 3 || argv[3] == "--socket"),
                "unknown daemon command"
            );
            ensure!(
                environment == BTreeMap::from([("RUST_LOG".into(), "info".into())]),
                "unknown daemon environment"
            );
            let home = PathBuf::from(&argv[2]);
            let layout = crate::service::Layout {
                agentd: argv[0].clone().into(),
                home: home.clone(),
                socket: argv.get(4).map(PathBuf::from),
                user_home: PathBuf::from("/"),
                uid: 0,
            };
            (
                crate::service::systemd_unit(&layout),
                crate::service::launchd_plist(&layout),
                agentdocker_core::paths::daemon_log(&home),
            )
        };
        // Accept pre-literal renderers only when expansion could not change any
        // value. Newline/specifier/variable-bearing old units stay opaque.
        let safe_legacy = argv
            .iter()
            .chain(environment.values())
            .all(|s| !s.contains(['%', '$']) && !s.chars().any(char::is_control));
        let old_quote = |s: &str| {
            if s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "/-._=:".contains(c))
            {
                s.to_owned()
            } else {
                format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
            }
        };
        let mut legacy_unit = String::new();
        if safe_legacy {
            for line in unit.lines() {
                if line.starts_with("ExecStart=") {
                    legacy_unit.push_str(&format!(
                        "ExecStart={}\n",
                        argv.iter()
                            .map(|s| old_quote(s))
                            .collect::<Vec<_>>()
                            .join(" ")
                    ));
                } else if connector && line.starts_with("Environment=") {
                    let key = if line.contains("AGENTDOCKER_HOME=") {
                        "AGENTDOCKER_HOME"
                    } else {
                        "PATH"
                    };
                    legacy_unit.push_str(&format!(
                        "Environment={key}={}\n",
                        old_quote(&environment[key])
                    ));
                } else {
                    legacy_unit.push_str(line);
                    legacy_unit.push('\n');
                }
            }
        }
        Ok(Self {
            argv,
            environment,
            log,
            unit,
            plist,
            legacy_unit,
            literal_command: true,
            connector,
        })
    }

    pub(super) fn include(
        &self,
        root: &Path,
        refs: &mut References,
        verified: &mut BTreeSet<PathBuf>,
    ) -> Result<()> {
        references::known_program(
            Path::new(&self.argv[0]),
            if self.connector {
                "agentdocker"
            } else {
                "agentd"
            },
            verified,
        )?;
        let mut paths = references::environment_paths(&self.environment)?;
        paths.push(self.argv[0].clone().into());
        paths.push(self.log.clone());
        if self.connector {
            let resources = crate::connector::service::argument_resources(&self.argv[3..])?;
            paths.extend(resources.executables);
            paths.extend(resources.data);
        } else {
            paths.push(self.argv[2].clone().into());
            paths.extend(self.argv.get(4).map(PathBuf::from));
        }
        for path in paths {
            refs.include(root, &path)?;
        }
        Ok(())
    }
}

fn read(path: &Path) -> Result<String> {
    let file = agentdocker_host::files::open_regular(path)?;
    ensure!(
        file.metadata()?.len() <= 64 * 1024,
        "oversized service definition"
    );
    let mut text = String::new();
    file.take(64 * 1024 + 1).read_to_string(&mut text)?;
    ensure!(
        text.len() <= 64 * 1024 && !text.contains('\0'),
        "invalid service definition"
    );
    Ok(text)
}

pub(super) fn plist(path: &Path, connector: bool) -> Result<Definition> {
    let text = read(path)?;
    let decoded = query(&[
        "/usr/bin/plutil".into(),
        "-convert".into(),
        "json".into(),
        "-o".into(),
        "-".into(),
        "--".into(),
        path.to_str().context("non-UTF8 service path")?.into(),
    ])?;
    let value: serde_json::Value = serde_json::from_str(&decoded)?;
    let argv = serde_json::from_value(value["ProgramArguments"].clone())?;
    let environment = serde_json::from_value(value["EnvironmentVariables"].clone())?;
    let definition = Definition::new(connector, argv, environment)?;
    ensure!(definition.plist == text, "unknown launchd definition");
    Ok(definition)
}

pub(super) fn unit(path: &Path, connector: bool) -> Result<Definition> {
    unit_text(&read(path)?, connector)
}

pub(super) fn unit_text(text: &str, connector: bool) -> Result<Definition> {
    let mut argv = None;
    let mut environment = BTreeMap::new();
    let mut literal_command = false;
    for line in text.lines() {
        if let Some(raw) = line.strip_prefix("ExecStart=") {
            ensure!(argv.is_none(), "multiple service commands");
            let mut words = words(raw)?;
            if let Some(command) = words.first_mut()
                && command.starts_with(':')
            {
                command.remove(0);
                literal_command = true;
            }
            argv = Some(words);
        } else if let Some(raw) = line.strip_prefix("Environment=") {
            let words = words(raw)?;
            ensure!(words.len() == 1, "multiple service assignments");
            let (key, value) = words[0]
                .split_once('=')
                .context("missing environment assignment")?;
            ensure!(
                environment
                    .insert(key.to_owned(), value.to_owned())
                    .is_none(),
                "duplicate environment assignment"
            );
        }
    }
    let mut result = Definition::new(
        connector,
        argv.context("missing service command")?,
        environment,
    )?;
    ensure!(
        if literal_command {
            result.unit == text
        } else {
            !result.legacy_unit.is_empty() && result.legacy_unit == text
        },
        "unknown systemd definition"
    );
    result.literal_command = literal_command;
    Ok(result)
}

/// Decode only the literal word syntax emitted by our old/current serializers.
/// The caller subsequently requires byte-for-byte renderer equality.
fn words(text: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            ' ' if !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            '\\' => {
                let escaped = match chars.next().context("truncated service escape")? {
                    '\\' => '\\',
                    '"' => '"',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'x' => {
                        let a = chars
                            .next()
                            .and_then(|v| v.to_digit(16))
                            .context("bad service escape")?;
                        let b = chars
                            .next()
                            .and_then(|v| v.to_digit(16))
                            .context("bad service escape")?;
                        char::from_u32(a * 16 + b).context("invalid service character")?
                    }
                    _ => bail!("unsupported service escape"),
                };
                word.push(escaped);
                started = true;
            }
            '%' => {
                ensure!(chars.next() == Some('%'), "service specifier expansion");
                word.push('%');
                started = true;
            }
            c => {
                ensure!(!c.is_control(), "unescaped service control character");
                word.push(c);
                started = true;
            }
        }
    }
    ensure!(!quoted, "unclosed service quote");
    if started {
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn emitted_units_round_trip_literals_and_reject_hidden_overrides() {
        let layout = crate::service::Layout {
            agentd: "/old %h ${HOME}/agentd".into(),
            home: "/state\nRestart=no".into(),
            socket: None,
            user_home: "/home".into(),
            uid: 0,
        };
        let text = crate::service::systemd_unit(&layout);
        let parsed = unit_text(&text, false).unwrap();
        assert_eq!(
            parsed.argv,
            ["/old %h ${HOME}/agentd", "--home", "/state\nRestart=no"]
        );
        for bad in [
            text.clone() + "RootDirectory=/old/build\n",
            text.replace("RestartSec=2", "RestartSec=2\nExecStartPre=/wrapper"),
            text.replace("%%h", "%h"),
        ] {
            assert!(unit_text(&bad, false).is_err());
        }
        let simple = Definition::new(
            false,
            vec!["/old/agentd".into(), "--home".into(), "/state".into()],
            BTreeMap::from([("RUST_LOG".into(), "info".into())]),
        )
        .unwrap();
        assert!(
            !unit_text(&simple.legacy_unit, false)
                .unwrap()
                .literal_command
        );
    }
}
