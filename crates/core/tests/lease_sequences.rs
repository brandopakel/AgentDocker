use agentdocker_core::{AgentId, LeaseId, LeaseMode, LeaseTable, ResourceKey};
use chrono::{Duration, Utc};
use proptest::prelude::*;

const PATHS: [&str; 6] = [
    "/repo",
    "/repo/src",
    "/repo/src/a",
    "/repo/src/b",
    "/repository",
    "/repo/doc",
];
#[derive(Clone)]
struct Expected {
    id: LeaseId,
    path: usize,
    owner: u8,
    shared: bool,
    expires: i64,
}
fn overlaps(a: usize, b: usize) -> bool {
    let a: Vec<_> = PATHS[a].split('/').collect();
    let b: Vec<_> = PATHS[b].split('/').collect();
    a.starts_with(&b) || b.starts_with(&a)
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn lease_sequences_match_reference(steps in prop::collection::vec((0u8..5, 0u8..4, 0usize..6, any::<bool>(), 1i64..8), 1..120)) {
        let mut table = LeaseTable::new();
        let mut expected: Vec<Expected> = Vec::new();
        let base = chrono::DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let mut clock = 0;
        for (op, owner, path, shared, ttl) in steps {
            let now = base + Duration::seconds(clock);
            table.expire(now);
            expected.retain(|l| l.expires > clock);
            let agent = AgentId::from(format!("agent-{owner}"));
            match op {
                0 | 1 => {
                    let blocked = expected.iter().any(|l| l.owner != owner && overlaps(l.path, path) && (!l.shared || !shared));
                    let result = table.claim(ResourceKey::new(format!("path:{}", PATHS[path])), agent.clone(), if shared {LeaseMode::Shared} else {LeaseMode::Exclusive}, Duration::seconds(ttl), None, now);
                    prop_assert_eq!(result.is_err(), blocked);
                    if let Ok(claimed) = result {
                        if let Some(l) = expected.iter_mut().find(|l| l.owner == owner && l.path == path && l.shared == shared) {
                            prop_assert_eq!(&l.id, &claimed.lease().id);
                            l.expires = clock + ttl;
                        } else {
                            expected.push(Expected {id: claimed.lease().id.clone(), path, owner, shared, expires: clock + ttl});
                        }
                    }
                }
                2 => { table.release_all(&agent); expected.retain(|l| l.owner != owner); }
                3 => { clock += ttl; table.expire(base + Duration::seconds(clock)); expected.retain(|l| l.expires > clock); }
                _ => {
                    if let Some(l) = expected.iter_mut().find(|l| l.path == path) {
                        let result = table.renew(&l.id, &agent, Duration::seconds(ttl), now);
                        prop_assert_eq!(result.is_ok(), l.owner == owner);
                        if result.is_ok() { l.expires = clock + ttl; }
                    }
                }
            }
            prop_assert_eq!(table.len(), expected.len());
            for l in &expected {
                let actual = table.get(&l.id).unwrap();
                prop_assert_eq!(actual.holder.as_str(), format!("agent-{}", l.owner));
                prop_assert_eq!(actual.resource.value(), PATHS[l.path]);
                prop_assert_eq!(actual.mode == LeaseMode::Shared, l.shared);
                prop_assert_eq!(actual.expires_at, base + Duration::seconds(l.expires));
            }
        }
    }
}
