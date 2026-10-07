//! Read-only manager inventory complements on-disk service definitions.
//! Keep conservative whole-store protection until exact executable references
//! and legacy registration races can be proved independently.
use super::*;

const DAEMON_LABEL: &str = crate::service::LABEL;
const CONNECTOR_LABEL: &str = crate::connector::service::LABEL;
const DAEMON_UNIT: &str = crate::service::UNIT;
const CONNECTOR_UNIT: &str = crate::connector::service::UNIT;

fn query(argv: &[String]) -> Result<String> {
    let output = command::run(Path::new("/"), argv, Duration::from_secs(10))
        .context("cannot inspect user service registrations; installation preserved")?;
    // Manager output may contain environment variables. Never echo it into
    // errors, maintenance JSON or logs, even when a query fails.
    ensure!(
        output.success,
        "cannot inspect user service registrations; installation preserved"
    );
    Ok(output.stdout)
}

pub(super) fn manager_references() -> Result<bool> {
    if cfg!(target_os = "macos") {
        let uid = crate::service::current_uid_for_service();
        for kind in ["gui", "user"] {
            let domain = format!("{kind}/{uid}");
            let text = query(&["/bin/launchctl".into(), "print".into(), domain.clone()])?;
            if launchd_references(&domain, &text)? {
                return Ok(true);
            }
        }
        Ok(false)
    } else if cfg!(target_os = "linux") {
        for method in ["ListUnits", "ListUnitFiles"] {
            let text = query(
                &[
                    "busctl",
                    "--user",
                    "--json=short",
                    "call",
                    "org.freedesktop.systemd1",
                    "/org/freedesktop/systemd1",
                    "org.freedesktop.systemd1.Manager",
                    method,
                ]
                .map(str::to_owned),
            )?;
            if systemd_references(method, &text)? {
                return Ok(true);
            }
        }
        Ok(false)
    } else {
        bail!("user service inventory is unavailable; installation preserved")
    }
}

fn launchd_references(domain: &str, text: &str) -> Result<bool> {
    ensure!(
        text.starts_with(&format!("{domain} = {{\n")) && text.trim_end().ends_with('}'),
        "unrecognized launchd domain inventory; installation preserved"
    );
    let mut tables = text.match_indices("\n\tservices = {\n");
    let (at, header) = tables
        .next()
        .context("missing launchd service inventory; installation preserved")?;
    ensure!(
        tables.next().is_none(),
        "ambiguous launchd service inventory; installation preserved"
    );
    let mut count = 0;
    let mut found = false;
    let mut closed = false;
    for row in text[at + header.len()..].lines() {
        if row == "\t}" {
            closed = true;
            break;
        }
        if row.trim().is_empty() {
            continue;
        }
        count += 1;
        let columns: Vec<_> = row.split_whitespace().collect();
        ensure!(
            count <= 10000 && columns.len() == 3 && columns[0].parse::<u32>().is_ok(),
            "unrecognized launchd service row; installation preserved"
        );
        // PID zero still denotes a loaded, stopped registration.
        found |= [DAEMON_LABEL, CONNECTOR_LABEL].contains(&columns[2]);
    }
    ensure!(
        closed,
        "incomplete launchd service inventory; installation preserved"
    );
    Ok(found)
}

fn systemd_references(method: &str, text: &str) -> Result<bool> {
    let (signature, width) = match method {
        "ListUnits" => ("a(ssssssouso)", 10),
        "ListUnitFiles" => ("a(ss)", 2),
        _ => bail!("unrecognized systemd inventory method"),
    };
    let value: serde_json::Value = serde_json::from_str(text)
        .context("unrecognized systemd service inventory; installation preserved")?;
    ensure!(
        value["type"] == signature && value["data"].as_array().is_some_and(|v| v.len() == 1),
        "unrecognized systemd service inventory; installation preserved"
    );
    let rows = value["data"][0]
        .as_array()
        .context("missing systemd service inventory; installation preserved")?;
    ensure!(
        rows.len() <= 10000,
        "systemd service inventory exceeds its bound"
    );
    let mut found = false;
    for row in rows {
        let columns = row
            .as_array()
            .context("unrecognized systemd service row; installation preserved")?;
        ensure!(
            columns.len() == width
                && columns.iter().enumerate().all(|(i, v)| {
                    if width == 10 && i == 7 {
                        v.is_u64()
                    } else {
                        v.is_string()
                    }
                }),
            "unrecognized systemd service row; installation preserved"
        );
        let name = columns[0].as_str().context("missing systemd unit name")?;
        let name = if method == "ListUnitFiles" {
            Path::new(name)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
        } else {
            name
        };
        // Installed (including disabled/masked) and cached-loaded units both
        // protect builds. Neither ActiveState nor a running PID is required.
        found |= [DAEMON_UNIT, CONNECTOR_UNIT].contains(&name);
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launchd_stopped_registration_is_present_without_a_live_pid() {
        let output = "gui/501 = {\n\tservices = {\n\t\t0 - dev.agentdocker.agentd\n\t\t0 (pe) unrelated\n\t}\n}\n";
        assert!(launchd_references("gui/501", output).unwrap());
        assert!(!launchd_references("gui/501", "gui/501 = {\n\tservices = {\n\t}\n}\n").unwrap());
        assert!(
            !launchd_references("gui/501", &output.replace(DAEMON_LABEL, "unrelated.daemon"))
                .unwrap()
        );
        for invalid in [
            output.replace("gui/501", "user/501"),
            output.replace("\n\t}", ""),
            output.replace("0 (pe)", "bad (pe)"),
            output.replace("services =", "unknown ="),
        ] {
            assert!(launchd_references("gui/501", &invalid).is_err());
        }
    }

    #[test]
    fn systemd_inventory_keeps_cached_stopped_and_alternate_unit_file_registrations() {
        let loaded = json!({"type":"a(ssssssouso)","data":[[[DAEMON_UNIT,"AgentDocker","loaded","inactive","dead","","/org/freedesktop/systemd1/unit/agentd_2eservice",0,"","/"]]]});
        assert!(systemd_references("ListUnits", &loaded.to_string()).unwrap());
        let installed = json!({"type":"a(ss)","data":[[[format!("/run/user/501/systemd/user/{CONNECTOR_UNIT}"),"disabled"]]]});
        assert!(systemd_references("ListUnitFiles", &installed.to_string()).unwrap());
        for (method, signature) in [("ListUnits", "a(ssssssouso)"), ("ListUnitFiles", "a(ss)")] {
            assert!(
                !systemd_references(method, &json!({"type":signature,"data":[[]]}).to_string())
                    .unwrap()
            );
        }
        for invalid in [
            json!({"type":"a(ss)","data":[[]]}),
            json!({"type":"a(ssssssouso)","data":[]}),
            json!({"type":"a(ssssssouso)","data":[[["unrelated"]]]}),
        ] {
            assert!(systemd_references("ListUnits", &invalid.to_string()).is_err());
        }
        let mut invalid_after_match = loaded;
        invalid_after_match["data"][0]
            .as_array_mut()
            .unwrap()
            .push(json!(["bad row"]));
        assert!(systemd_references("ListUnits", &invalid_after_match.to_string()).is_err());
    }
}
