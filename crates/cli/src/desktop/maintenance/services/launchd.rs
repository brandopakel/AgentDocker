//! A loaded registration must agree with an exact recognized persisted plist.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

fn block(text: &str, name: &str) -> Result<Vec<String>> {
    let header = format!("\n\t{name} = {{\n");
    let mut matches = text.match_indices(&header);
    let Some((at, _)) = matches.next() else {
        return Ok(Vec::new());
    };
    ensure!(matches.next().is_none(), "ambiguous launchd block");
    let mut lines = Vec::new();
    for line in text[at + header.len()..].lines() {
        if line == "\t}" {
            return Ok(lines);
        }
        let value = line
            .strip_prefix("\t\t")
            .context("unrecognized launchd block")?;
        ensure!(
            !value.chars().any(char::is_control) && !value.contains(['\\', '"', '\u{fffd}']),
            "ambiguous launchd value"
        );
        lines.push(value.to_owned());
    }
    bail!("incomplete launchd block")
}

fn scalar<'a>(text: &'a str, name: &str) -> Result<Option<&'a str>> {
    let prefix = format!("\t{name} = ");
    let mut matches = text.lines().filter_map(|l| l.strip_prefix(&prefix));
    let value = matches.next();
    ensure!(matches.next().is_none(), "ambiguous launchd field");
    if let Some(value) = value {
        ensure!(
            !value.contains(['\\', '"', '\u{fffd}']) && !value.chars().any(char::is_control),
            "ambiguous launchd scalar"
        );
    }
    Ok(value)
}

fn environment(text: &str) -> Result<BTreeMap<String, String>> {
    let mut env = BTreeMap::new();
    for section in [
        "inherited environment",
        "default environment",
        "environment",
    ] {
        let mut section_keys = BTreeSet::new();
        for line in block(text, section)? {
            let (key, value) = line
                .split_once(" => ")
                .context("invalid launchd environment")?;
            ensure!(
                section_keys.insert(key.to_owned()),
                "duplicate launchd environment"
            );
            env.insert(key.to_owned(), value.to_owned());
        }
    }
    Ok(env)
}

fn cached(text: &str, target: &str, definition: &definition::Definition) -> Result<Vec<PathBuf>> {
    ensure!(
        text.starts_with(&format!("{target} = {{\n")) && text.trim_end().ends_with('}'),
        "unknown launchd job"
    );
    for line in text
        .lines()
        .filter(|line| line.starts_with('\t') && !line.starts_with("\t\t"))
    {
        let Some((key, _)) = line.trim_start_matches('\t').split_once(" = ") else {
            continue;
        };
        ensure!(
            matches!(
                key,
                "active count"
                    | "path"
                    | "type"
                    | "state"
                    | "program"
                    | "BTM uuid"
                    | "arguments"
                    | "stdout path"
                    | "stderr path"
                    | "working directory"
                    | "inherited environment"
                    | "default environment"
                    | "environment"
                    | "domain"
                    | "asid"
                    | "minimum runtime"
                    | "exit timeout"
                    | "runs"
                    | "pid"
                    | "immediate reason"
                    | "forks"
                    | "execs"
                    | "initialized"
                    | "trampolined"
                    | "started suspended"
                    | "proxy started suspended"
                    | "checked allocations"
                    | "checked allocations reason"
                    | "checked allocations flags"
                    | "last exit code"
                    | "last terminating signal"
                    | "semaphores"
                    | "resource coalition"
                    | "jetsam coalition"
                    | "spawn type"
                    | "jetsam priority"
                    | "jetsam memory limit (active)"
                    | "jetsam memory limit (inactive)"
                    | "jetsamproperties category"
                    | "jetsam thread limit"
                    | "cpumon"
                    | "properties"
            ),
            "unknown cached launchd setting"
        );
    }
    ensure!(
        block(text, "arguments")? == definition.argv
            && scalar(text, "program")? == Some(&definition.argv[0]),
        "cached launchd command differs from definition"
    );
    let env = environment(text)?;
    ensure!(
        definition
            .environment
            .iter()
            .all(|(k, v)| env.get(k) == Some(v)),
        "cached launchd environment differs"
    );
    let mut paths = references::environment_paths(&env)?;
    for key in ["stdout path", "stderr path", "working directory"] {
        if let Some(path) = scalar(text, key)? {
            paths.push(path.into());
        }
    }
    Ok(paths)
}

pub(super) fn inventory(layout: &Layout, homes: &[PathBuf]) -> Result<References> {
    let uid = crate::service::current_uid_for_service();
    let mut loaded = Vec::new();
    let mut environments = Vec::new();
    for kind in ["gui", "user"] {
        let domain = format!("{kind}/{uid}");
        let text = query(&["/bin/launchctl".into(), "print".into(), domain.clone()])?;
        let present = super::launchd_references(&domain, &text)?;
        environments.push(text.clone());
        if present {
            // The validator has bounded and checked the entire service table.
            let table = text
                .split_once("\n\tservices = {\n")
                .context("missing service table")?
                .1
                .split_once("\n\t}")
                .context("incomplete service table")?
                .0;
            for row in table.lines() {
                let columns: Vec<_> = row.split_whitespace().collect();
                if columns.len() == 3 && [DAEMON_LABEL, CONNECTOR_LABEL].contains(&columns[2]) {
                    loaded.push((
                        format!("{domain}/{}", columns[2]),
                        columns[2] == CONNECTOR_LABEL,
                    ));
                }
            }
        }
    }
    let mut files = BTreeMap::new();
    for home in homes {
        for (label, connector) in [(DAEMON_LABEL, false), (CONNECTOR_LABEL, true)] {
            let path = home
                .join("Library/LaunchAgents")
                .join(format!("{label}.plist"));
            match path.symlink_metadata() {
                Ok(_) => {
                    files.insert(path, connector);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Ok(true.into()),
            }
        }
    }
    if files.is_empty() && loaded.is_empty() {
        return Ok(References::default());
    }
    let recognized = (|| -> Result<References> {
        let mut refs = References::default();
        let mut verified = BTreeSet::new();
        for text in environments {
            for path in references::environment_paths(&environment(&text)?)? {
                refs.include(&layout.root, &path)?;
            }
        }
        for (path, connector) in files {
            definition::plist(&path, connector)?.include(&layout.root, &mut refs, &mut verified)?;
            refs.include(&layout.root, &path)?;
        }
        for (target, connector) in loaded {
            let text = query(&["/bin/launchctl".into(), "print".into(), target.clone()])?;
            let path = scalar(&text, "path")?.context("missing cached plist path")?;
            ensure!(Path::new(path).is_absolute(), "relative cached plist");
            let definition = definition::plist(Path::new(path), connector)?;
            for path in cached(&text, &target, &definition)? {
                refs.include(&layout.root, &path)?;
            }
            definition.include(&layout.root, &mut refs, &mut verified)?;
            refs.include(&layout.root, Path::new(path))?;
        }
        Ok(refs)
    })();
    Ok(recognized.unwrap_or_else(|_| true.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stopped_cached_job_must_match_arguments_environment_and_known_settings() {
        let layout = crate::service::Layout {
            agentd: "/old/agentd".into(),
            home: "/state".into(),
            socket: None,
            user_home: "/home".into(),
            uid: 0,
        };
        let definition =
            definition::unit_text(&crate::service::systemd_unit(&layout), false).unwrap();
        let text = "gui/501/dev.agentdocker.agentd = {\n\tprogram = /old/agentd\n\targuments = {\n\t\t/old/agentd\n\t\t--home\n\t\t/state\n\t}\n\tenvironment = {\n\t\tRUST_LOG => info\n\t}\n\tpid = 0\n\tstdout path = /state/agentd.log\n}\n";
        assert_eq!(
            cached(text, "gui/501/dev.agentdocker.agentd", &definition).unwrap(),
            [PathBuf::from("/state/agentd.log")]
        );
        for changed in [
            text.replace("/old/agentd", "/different/agentd"),
            text.replace("RUST_LOG => info", "RUST_LOG => debug"),
            text.replace("\tpid = 0", "\troot directory = /old/version"),
            text.replace(
                "RUST_LOG => info",
                "RUST_LOG => info\n\t\tDYLD_INSERT_LIBRARIES => /old/plugin",
            ),
            text.replace("\t\t/state", "\t\t\"/state\""),
        ] {
            assert!(cached(&changed, "gui/501/dev.agentdocker.agentd", &definition).is_err());
        }
    }
}
