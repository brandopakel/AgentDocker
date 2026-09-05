#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    if let Ok(request) = serde_json::from_slice::<agentdocker_core::Request>(data) {
        let encoded = serde_json::to_vec(&request).unwrap();
        let decoded: agentdocker_core::Request = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), serde_json::to_value(decoded).unwrap());
    }
});
