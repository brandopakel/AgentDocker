use agentdocker_core::{AgentId, LeaseMode, LeaseTable, ResourceKey};
use chrono::{Duration, Utc};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
fn leases(c: &mut Criterion) {
    let now = chrono::DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
    let mut group = c.benchmark_group("lease_conflict");
    for size in [1, 100, 1000] {
        let mut table = LeaseTable::new();
        for n in 0..size {
            table
                .claim(
                    ResourceKey::new(format!("path:/repo/{n}")),
                    AgentId::from("owner"),
                    LeaseMode::Exclusive,
                    Duration::seconds(60),
                    None,
                    now,
                )
                .unwrap();
        }
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                black_box(
                    table
                        .claim(
                            ResourceKey::new("path:/repo"),
                            AgentId::from("contender"),
                            LeaseMode::Exclusive,
                            Duration::seconds(60),
                            None,
                            now,
                        )
                        .unwrap_err(),
                );
            })
        });
    }
    group.finish();
}
criterion_group!(benches, leases);
criterion_main!(benches);
