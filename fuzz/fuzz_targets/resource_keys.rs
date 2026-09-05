#![no_main]
use agentdocker_core::ResourceKey;
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &str| {
    let (a, b) = data.split_once('\n').unwrap_or((data, "path:/"));
    let a = ResourceKey::new(a);
    let b = ResourceKey::new(b);
    assert_eq!(a, ResourceKey::new(a.as_str()));
    assert_eq!(a.overlaps(&b), b.overlaps(&a));
    assert!(a.overlaps(&a));
});
