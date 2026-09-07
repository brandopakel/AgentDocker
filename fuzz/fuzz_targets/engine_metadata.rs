#![no_main]
use agentdocker_core::container::{ContainerEngine, ContainerIntent, ManagedContainer};
use agentdocker_core::{AgentRecord, AgentSpec};
use agentdocker_host::containers::{ContainerState, parse_inspection};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;
use std::sync::OnceLock;

fn record() -> &'static AgentRecord {
    static RECORD: OnceLock<AgentRecord> = OnceLock::new();
    RECORD.get_or_init(|| {
        let mut record = AgentRecord::new(
            AgentSpec::default(),
            true,
            chrono::DateTime::from_timestamp(0, 0).unwrap(),
        );
        record.id = "metadata-fixture".into();
        record.container = Some(ManagedContainer {
            inputs: None,
            engine: ContainerEngine::Docker,
            connection: None,
            build: "fixture-build".into(),
            image_id: format!("sha256:{}", "a".repeat(64)),
            name: "fixture".into(),
            owner: "fixture-owner".into(),
            id: Some("b".repeat(64)),
            intent: ContainerIntent::Run,
            start_attempted: true,
            create_attempted: true,
            last_error: None,
            options: Default::default(),
            workspace: None,
            deadline: None,
        });
        record
    })
}

fuzz_target!(|data: &str| {
    let Ok(inspection) = parse_inspection(record(), data) else {
        return;
    };
    let raw: Value = serde_json::from_str(data).unwrap();
    assert_eq!(raw.as_array().unwrap().len(), 1);
    let item = &raw[0];
    assert_eq!(inspection.id, "b".repeat(64));
    assert_eq!(
        item["Config"]["Labels"]["org.agentdocker.owner"],
        "fixture-owner"
    );
    assert_eq!(
        item["Config"]["Labels"]["org.agentdocker.agent"],
        "metadata-fixture"
    );
    assert_eq!(
        item["Config"]["Labels"]["org.agentdocker.build"],
        "fixture-build"
    );
    assert_eq!(item["HostConfig"]["AutoRemove"], false);
    assert!(matches!(
        item["HostConfig"]["RestartPolicy"]["Name"].as_str(),
        Some("no" | "")
    ));
    if let ContainerState::Exited(code) = inspection.state {
        assert_eq!(item["State"]["Running"], false);
        assert_eq!(item["State"]["Restarting"], false);
        assert_eq!(item["State"]["Pid"], 0);
        assert_eq!(item["State"]["ExitCode"].as_i64(), Some(i64::from(code)));
    }
    // Independent identity mutations must never keep an accepted observation.
    for pointer in [
        "/0/Id",
        "/0/Image",
        "/0/Config/Labels/org.agentdocker.owner",
        "/0/Config/Labels/org.agentdocker.agent",
        "/0/Config/Labels/org.agentdocker.build",
    ] {
        let mut altered = raw.clone();
        *altered.pointer_mut(pointer).unwrap() = Value::String("foreign".into());
        assert!(parse_inspection(record(), &altered.to_string()).is_err());
    }
});
