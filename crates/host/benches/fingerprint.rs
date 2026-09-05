use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
fn fingerprints(c: &mut Criterion) {
    let root = tempfile::tempdir().unwrap();
    for n in 0..100 {
        std::fs::write(root.path().join(format!("file-{n}.rs")), vec![b'x'; 4096]).unwrap();
    }
    c.bench_function("fingerprint/100_files_400_kib", |b| {
        b.iter(|| black_box(agentdocker_host::content::fingerprint(root.path()).unwrap()))
    });
}
criterion_group!(benches, fingerprints);
criterion_main!(benches);
