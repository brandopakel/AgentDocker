//! Match persisted unit renderers against the manager's independently cached data.
use super::*;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

fn call(path: &str, interface: &str, method: &str, args: &[&str]) -> Result<Value> {
    let mut argv = [
        "busctl",
        "--user",
        "--json=short",
        "call",
        "org.freedesktop.systemd1",
        path,
        interface,
        method,
    ]
    .map(str::to_owned)
    .to_vec();
    argv.extend(args.iter().map(|v| (*v).to_owned()));
    Ok(serde_json::from_str(&query(&argv)?)?)
}

fn properties(path: &str, interface: &str) -> Result<Value> {
    let value = call(
        path,
        "org.freedesktop.DBus.Properties",
        "GetAll",
        &["s", interface],
    )?;
    ensure!(
        value["type"] == "a{sv}"
            && value["data"].as_array().is_some_and(|v| v.len() == 1)
            && value["data"][0].is_object(),
        "invalid service properties"
    );
    Ok(value["data"][0].clone())
}

fn property<'a>(value: &'a Value, key: &str, signature: &str) -> Result<&'a Value> {
    let item = &value[key];
    ensure!(
        item["type"] == signature && item.get("data").is_some(),
        "invalid cached service property"
    );
    Ok(&item["data"])
}

fn strings(value: &Value) -> Result<Vec<String>> {
    Ok(serde_json::from_value(value.clone())?)
}

/// A negative lookup cached by `systemctl show` is not a registration. Require
/// the full empty/inactive shape, so deleted files on real cached units and any
/// remaining command, override, job or process still preserve the installation.
fn cached_fragment<'a>(unit: &'a Value, service: &Value) -> Result<Option<&'a str>> {
    if property(unit, "LoadState", "s")? == "not-found" {
        ensure!(
            property(unit, "ActiveState", "s")? == "inactive"
                && property(unit, "SubState", "s")? == "dead"
                && property(unit, "Job", "(uo)")? == &json!([0, "/"])
                && property(unit, "NeedDaemonReload", "b")? == &json!(false)
                && property(unit, "Transient", "b")? == &json!(false),
            "ambiguous missing service"
        );
        for key in ["FragmentPath", "SourcePath"] {
            ensure!(
                property(unit, key, "s")? == "",
                "missing service has a definition"
            );
        }
        ensure!(
            strings(property(unit, "DropInPaths", "as")?)?.is_empty(),
            "missing service has overrides"
        );
        for key in ["MainPID", "ControlPID"] {
            ensure!(
                property(service, key, "u")? == &json!(0),
                "missing service has a process"
            );
        }
        for key in [
            "ExecCondition",
            "ExecStartPre",
            "ExecStart",
            "ExecStartPost",
            "ExecReload",
            "ExecStop",
            "ExecStopPost",
        ] {
            for (name, signature) in [
                (key.to_owned(), "a(sasbttttuii)"),
                (format!("{key}Ex"), "a(sasasttttuii)"),
            ] {
                ensure!(
                    property(service, &name, signature)?
                        .as_array()
                        .is_some_and(Vec::is_empty),
                    "missing service has a command"
                );
            }
        }
        for (key, signature) in [
            ("Environment", "as"),
            ("EnvironmentFiles", "a(sb)"),
            ("BindPaths", "a(ssbt)"),
            ("BindReadOnlyPaths", "a(ssbt)"),
        ] {
            ensure!(
                property(service, key, signature)?
                    .as_array()
                    .is_some_and(Vec::is_empty),
                "missing service has resource settings"
            );
        }
        for key in ["WorkingDirectory", "RootDirectory", "RootImage"] {
            ensure!(
                property(service, key, "s")? == "",
                "missing service has a filesystem setting"
            );
        }
        return Ok(None);
    }
    ensure!(
        property(unit, "LoadState", "s")? == "loaded",
        "unavailable service definition"
    );
    let fragment = property(unit, "FragmentPath", "s")?
        .as_str()
        .context("invalid cached fragment")?;
    ensure!(
        Path::new(fragment).is_absolute(),
        "missing cached definition"
    );
    Ok(Some(fragment))
}

fn cached_matches(
    unit: &Value,
    service: &Value,
    definition: &definition::Definition,
) -> Result<Vec<PathBuf>> {
    ensure!(
        property(unit, "NeedDaemonReload", "b")? == &json!(false)
            && property(unit, "Transient", "b")? == &json!(false),
        "changed or transient service"
    );
    ensure!(
        property(unit, "SourcePath", "s")? == ""
            && strings(property(unit, "DropInPaths", "as")?)?.is_empty(),
        "service overrides"
    );
    let commands = property(service, "ExecStart", "a(sasbttttuii)")?
        .as_array()
        .context("missing cached command")?;
    ensure!(commands.len() == 1, "multiple cached commands");
    let command = commands[0].as_array().context("invalid cached command")?;
    ensure!(
        command.len() == 10
            && command[0] == definition.argv[0]
            && strings(&command[1])? == definition.argv
            && command[2] == false,
        "cached command differs from definition"
    );
    let extended = property(service, "ExecStartEx", "a(sasasttttuii)")?
        .as_array()
        .context("missing cached command flags")?;
    ensure!(
        extended.len() == 1
            && extended[0].as_array().is_some_and(|r| r.len() == 10)
            && extended[0][0] == command[0]
            && extended[0][1] == command[1],
        "cached extended command differs"
    );
    let flags = strings(&extended[0][2])?;
    ensure!(
        if definition.literal_command {
            flags == ["no-env-expand"]
        } else {
            flags.is_empty()
        },
        "unknown cached command flags"
    );
    for name in [
        "ExecCondition",
        "ExecStartPre",
        "ExecStartPost",
        "ExecReload",
        "ExecStop",
        "ExecStopPost",
    ] {
        ensure!(
            property(service, name, "a(sasbttttuii)")?
                .as_array()
                .is_some_and(Vec::is_empty),
            "additional cached command"
        );
    }
    ensure!(
        property(service, "EnvironmentFiles", "a(sb)")?
            .as_array()
            .is_some_and(Vec::is_empty),
        "environment file override"
    );
    for name in ["RootDirectory", "RootImage"] {
        ensure!(
            property(service, name, "s")? == "",
            "changed service filesystem root"
        );
    }
    for name in ["BindPaths", "BindReadOnlyPaths"] {
        ensure!(
            property(service, name, "a(ssbt)")?
                .as_array()
                .is_some_and(Vec::is_empty),
            "service bind override"
        );
    }
    let mut environment = BTreeMap::new();
    for entry in strings(property(service, "Environment", "as")?)? {
        let (key, value) = entry
            .split_once('=')
            .context("invalid cached environment")?;
        ensure!(
            environment
                .insert(key.to_owned(), value.to_owned())
                .is_none(),
            "duplicate cached environment"
        );
    }
    ensure!(
        environment == definition.environment,
        "cached environment differs from definition"
    );
    let mut paths = Vec::new();
    let cwd = property(service, "WorkingDirectory", "s")?
        .as_str()
        .context("invalid cached working directory")?;
    if !cwd.is_empty() {
        // systemd's property_get_working_directory serializes missing-ok as
        // a leading '!'. It still names a dependency, even when absent today.
        // Home-relative '~' and unknown forms remain conservative.
        let cwd = cwd.strip_prefix('!').unwrap_or(cwd);
        ensure!(
            Path::new(cwd).is_absolute(),
            "ambiguous cached working directory"
        );
        paths.push(cwd.into());
    }
    Ok(paths)
}

pub(super) fn inventory(layout: &Layout, homes: &[PathBuf]) -> Result<References> {
    let mut files = BTreeMap::new();
    let mut loaded = Vec::new();
    for method in ["ListUnits", "ListUnitFiles"] {
        let value = call(
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
            method,
            &[],
        )?;
        // Validate the full table before using any row, including rows after a match.
        super::systemd_references(method, &value.to_string())?;
        for row in value["data"][0]
            .as_array()
            .context("missing unit inventory")?
        {
            let raw = row[0].as_str().context("missing unit name")?;
            let name = Path::new(raw)
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or("");
            let connector = match name {
                DAEMON_UNIT => false,
                CONNECTOR_UNIT => true,
                _ => continue,
            };
            if method == "ListUnits" {
                loaded.push((
                    row[6].as_str().context("missing unit object")?.to_owned(),
                    connector,
                ));
            } else {
                if !Path::new(raw).is_absolute() {
                    return Ok(true.into());
                }
                files.insert(PathBuf::from(raw), connector);
            }
        }
    }
    for home in homes {
        for (name, connector) in [(DAEMON_UNIT, false), (CONNECTOR_UNIT, true)] {
            let path = home.join(".config/systemd/user").join(name);
            match path.symlink_metadata() {
                Ok(_) => {
                    files.insert(path, connector);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Ok(true.into()),
            }
        }
    }
    ensure!(
        files.len() <= 8 && loaded.len() <= 2,
        "ambiguous service inventory"
    );
    if files.is_empty() && loaded.is_empty() {
        return Ok(References::default());
    }
    let recognized = (|| -> Result<References> {
        let mut refs = References::default();
        let mut verified = BTreeSet::new();
        let mut has_registration = !files.is_empty();
        for (path, connector) in files {
            let definition = definition::unit(&path, connector)?;
            definition.include(&layout.root, &mut refs, &mut verified)?;
            refs.include(&layout.root, &path)?;
        }
        for (object, connector) in loaded {
            let unit = properties(&object, "org.freedesktop.systemd1.Unit")?;
            let service = properties(&object, "org.freedesktop.systemd1.Service")?;
            let Some(fragment) = cached_fragment(&unit, &service)? else {
                continue;
            };
            has_registration = true;
            let definition = definition::unit(Path::new(fragment), connector)?;
            for path in cached_matches(&unit, &service, &definition)? {
                refs.include(&layout.root, &path)?;
            }
            definition.include(&layout.root, &mut refs, &mut verified)?;
            refs.include(&layout.root, Path::new(fragment))?;
        }
        if has_registration {
            let mut manager_environment = BTreeMap::new();
            let manager = properties(
                "/org/freedesktop/systemd1",
                "org.freedesktop.systemd1.Manager",
            )?;
            for entry in strings(property(&manager, "Environment", "as")?)? {
                let (key, value) = entry
                    .split_once('=')
                    .context("invalid manager environment")?;
                ensure!(
                    manager_environment
                        .insert(key.to_owned(), value.to_owned())
                        .is_none(),
                    "duplicate manager environment"
                );
            }
            for path in references::environment_paths(&manager_environment)? {
                refs.include(&layout.root, &path)?;
            }
        }
        Ok(refs)
    })();
    // Output can contain secrets; never include the parser/manager value in diagnostics.
    Ok(recognized.unwrap_or_else(|_| true.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn variant(signature: &str, value: Value) -> Value {
        json!({"type":signature,"data":value})
    }
    fn fixture() -> (definition::Definition, Value, Value) {
        let layout = crate::service::Layout {
            agentd: "/old/agentd".into(),
            home: "/state".into(),
            socket: None,
            user_home: "/home".into(),
            uid: 0,
        };
        let definition =
            definition::unit_text(&crate::service::systemd_unit(&layout), false).unwrap();
        let unit = json!({"NeedDaemonReload":variant("b",json!(false)),"Transient":variant("b",json!(false)),"SourcePath":variant("s",json!("")),"DropInPaths":variant("as",json!([]))});
        let mut service = json!({
            "ExecStart":variant("a(sasbttttuii)",json!([["/old/agentd",definition.argv,false,0,0,0,0,0,0,0]])),
            "ExecStartEx":variant("a(sasasttttuii)",json!([["/old/agentd",definition.argv,["no-env-expand"],0,0,0,0,0,0,0]])),
            "Environment":variant("as",json!(["RUST_LOG=info"])),
            "EnvironmentFiles":variant("a(sb)",json!([])),
            "RootDirectory":variant("s",json!("")),"RootImage":variant("s",json!("")),
            "BindPaths":variant("a(ssbt)",json!([])),"BindReadOnlyPaths":variant("a(ssbt)",json!([])),
            "WorkingDirectory":variant("s",json!("/work"))});
        for name in [
            "ExecCondition",
            "ExecStartPre",
            "ExecStartPost",
            "ExecReload",
            "ExecStop",
            "ExecStopPost",
        ] {
            service[name] = variant("a(sasbttttuii)", json!([]));
        }
        (definition, unit, service)
    }
    #[test]
    fn missing_systemd_status_record_has_no_registration() {
        // Real systemd keeps these negative lookup objects after `show` of an
        // absent unit. They have no persisted file or executable to retain.
        let mut unit = json!({
            "LoadState":variant("s",json!("not-found")),
            "ActiveState":variant("s",json!("inactive")),
            "SubState":variant("s",json!("dead")),
            "FragmentPath":variant("s",json!("")),
            "SourcePath":variant("s",json!("")),
            "DropInPaths":variant("as",json!([])),
            "NeedDaemonReload":variant("b",json!(false)),
            "Transient":variant("b",json!(false)),
            "Job":variant("(uo)",json!([0,"/"]))});
        let mut service = json!({
            "MainPID":variant("u",json!(0)),"ControlPID":variant("u",json!(0)),
            "Environment":variant("as",json!([])),
            "EnvironmentFiles":variant("a(sb)",json!([])),
            "WorkingDirectory":variant("s",json!("")),
            "RootDirectory":variant("s",json!("")),"RootImage":variant("s",json!("")),
            "BindPaths":variant("a(ssbt)",json!([])),"BindReadOnlyPaths":variant("a(ssbt)",json!([]))});
        for key in [
            "ExecCondition",
            "ExecStartPre",
            "ExecStart",
            "ExecStartPost",
            "ExecReload",
            "ExecStop",
            "ExecStopPost",
        ] {
            service[key] = variant("a(sasbttttuii)", json!([]));
            service[format!("{key}Ex")] = variant("a(sasasttttuii)", json!([]));
        }
        assert_eq!(cached_fragment(&unit, &service).unwrap(), None);
        for (key, value) in [
            ("FragmentPath", json!("/old/service")),
            ("SourcePath", json!("/old/source")),
            ("LoadState", json!("loaded")),
            ("ActiveState", json!("activating")),
            ("Job", json!([3, "/job/3"])),
            ("DropInPaths", json!(["/old/override"])),
        ] {
            let mut changed = unit.clone();
            changed[key]["data"] = value;
            assert!(cached_fragment(&changed, &service).is_err(), "{key}");
        }
        for (key, value) in [
            ("MainPID", json!(123)),
            ("ControlPID", json!(123)),
            ("WorkingDirectory", json!("/old/version")),
            ("Environment", json!(["PATH=/old/version"])),
            (
                "ExecCondition",
                json!([["/old/check", [], false, 0, 0, 0, 0, 0, 0, 0]]),
            ),
        ] {
            let mut changed = service.clone();
            changed[key]["data"] = value;
            assert!(cached_fragment(&unit, &changed).is_err(), "{key}");
        }
        unit["LoadState"]["data"] = json!("loaded");
        assert!(
            cached_fragment(&unit, &service).is_err(),
            "real cached service with missing fragment remains conservative"
        );
    }

    #[test]
    fn cached_optional_working_directory_retains_its_version() {
        // systemd's D-Bus getter prefixes a missing-ok directory with '!'.
        // The real Oracle user manager supplies this for its default home.
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("store");
        let id = "a".repeat(64);
        let cwd = root.join("versions").join(&id).join("future/work");
        let (definition, unit, mut service) = fixture();
        for prefix in ["", "!"] {
            service["WorkingDirectory"]["data"] = json!(format!("{prefix}{}", cwd.display()));
            let paths = cached_matches(&unit, &service, &definition).unwrap();
            let mut refs = References::default();
            for path in &paths {
                refs.include(&root, path).unwrap();
            }
            assert_eq!(paths, [cwd.clone()]);
            assert!(refs.retains(&id));
            assert!(!refs.retains(&"b".repeat(64)));
            assert!(
                !cwd.exists(),
                "inventory never creates the optional directory"
            );
        }
        for invalid in [
            "!",
            "!!/work",
            "!relative",
            "relative",
            "~",
            "!~",
            "!/old/../work",
        ] {
            service["WorkingDirectory"]["data"] = json!(invalid);
            let result = cached_matches(&unit, &service, &definition).and_then(|paths| {
                let mut refs = References::default();
                for path in paths {
                    refs.include(&root, &path)?;
                }
                Ok(refs)
            });
            assert!(result.is_err(), "ambiguous cwd must retain all: {invalid}");
        }
        service["WorkingDirectory"]["data"] = json!("");
        assert!(
            cached_matches(&unit, &service, &definition)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn stopped_cached_unit_agrees_but_changed_files_or_overrides_do_not() {
        let (definition, unit, service) = fixture();
        assert_eq!(
            cached_matches(&unit, &service, &definition).unwrap(),
            [PathBuf::from("/work")]
        );
        for (key, value) in [
            ("NeedDaemonReload", json!(true)),
            ("Transient", json!(true)),
            ("DropInPaths", json!(["/old/override.conf"])),
            ("SourcePath", json!("/generated/source")),
        ] {
            let mut changed = unit.clone();
            changed[key]["data"] = value;
            assert!(
                cached_matches(&changed, &service, &definition).is_err(),
                "{key}"
            );
        }
        for (key, value) in [
            (
                "Environment",
                json!(["RUST_LOG=info", "AGENTDOCKER_HOME=/different"]),
            ),
            ("RootDirectory", json!("/old/root")),
            ("EnvironmentFiles", json!([["/old/environment", false]])),
            ("BindPaths", json!([["/old/path", "/work", false, 0]])),
            (
                "ExecStartPre",
                json!([["/wrapper", [], false, 0, 0, 0, 0, 0, 0, 0]]),
            ),
        ] {
            let mut changed = service.clone();
            changed[key]["data"] = value;
            assert!(
                cached_matches(&unit, &changed, &definition).is_err(),
                "{key}"
            );
        }
        let mut changed = service.clone();
        changed["ExecStart"]["data"][0][1][0] = json!("/different/agentd");
        assert!(cached_matches(&unit, &changed, &definition).is_err());
        let mut changed = service;
        changed["ExecStartEx"]["data"][0][2] = json!(["ignore-failure"]);
        assert!(cached_matches(&unit, &changed, &definition).is_err());
    }
}
