use agentdocker_host::usage::reader::{Budget, Runtime, Session, Stop};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::{hint::black_box, io::Write, time::Duration};

/// Measure content fingerprinting over a fixed set of small files.
fn fingerprints(c: &mut Criterion) {
    let root = tempfile::tempdir().unwrap();
    for n in 0..100 {
        std::fs::write(root.path().join(format!("file-{n}.rs")), vec![b'x'; 4096]).unwrap();
    }
    c.bench_function("fingerprint/100_files_400_kib", |b| {
        b.iter(|| black_box(agentdocker_host::content::fingerprint(root.path()).unwrap()))
    });
}

/// Stop an overloaded benchmark instead of retrying stalled work forever.
fn record_progress(stalled: &mut u8, made_progress: bool) {
    if made_progress {
        *stalled = 0;
    } else {
        *stalled += 1;
        assert!(
            *stalled < 10,
            "accounting prefix benchmark made no progress for ten consecutive attempts"
        );
    }
}

/// Measure verification of the retained prefix through the bounded reader API.
fn usage_prefix(c: &mut Criterion) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("synthetic.jsonl");
    let prefix = b"{\"type\":\"user\",\"message\":{\"content\":\"";
    let suffix = b"\"}}\n";
    let mut line = prefix.to_vec();
    line.resize(64 * 1024 - suffix.len(), b'x');
    line.extend_from_slice(suffix);
    let mut file = std::fs::File::create(&path).unwrap();
    for _ in 0..256 {
        file.write_all(&line).unwrap();
    }
    drop(file);

    // Establish the same complete-record cursor a collector retains. Parsing
    // and fixture creation are outside the measured prefix-verification work.
    let mut initial = Session::open(&path, Runtime::Claude, None).unwrap();
    assert!(
        initial
            .prepare_next(&path, 4 * 1024 * 1024, Duration::from_millis(100))
            .unwrap()
            .ready
    );
    let mut stalled = 0;
    let cursor = loop {
        let previous = initial.offset();
        let batch = initial.scan(&path, Budget::default()).unwrap();
        assert!(batch.gaps.is_empty());
        if batch.stop == Stop::Complete {
            break batch.cursor;
        }
        record_progress(&mut stalled, batch.cursor.offset() > previous);
    };
    assert_eq!(cursor.offset(), 16 * 1024 * 1024);
    let mut group = c.benchmark_group("usage_prefix");
    group.throughput(Throughput::Bytes(cursor.offset()));
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
    group.bench_function("verify_16_mib", |b| {
        b.iter(|| {
            let mut session = Session::open(&path, Runtime::Claude, Some(&cursor)).unwrap();
            let mut stalled = 0;
            loop {
                let progress = session
                    .prepare_next(&path, 4 * 1024 * 1024, Duration::from_millis(100))
                    .unwrap();
                if progress.ready {
                    break;
                }
                record_progress(&mut stalled, progress.bytes_read != 0);
            }
            black_box(session.offset())
        })
    });
    group.finish();
}

criterion_group!(benches, fingerprints, usage_prefix);
criterion_main!(benches);
