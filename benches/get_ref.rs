use std::time::Duration;
use criterion::{Criterion, criterion_group, criterion_main, black_box};
use ref_count::RefCount;

fn get_ref(c: &mut Criterion) {
    let rc = RefCount::<32>::new();
    let tokio_rw_lock = tokio::sync::RwLock::new(());

    let mut write = c.benchmark_group("get-write");

    write.bench_function("ref_count", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::new(0, 0);
            for _ in 0..iters {
                let start = std::time::Instant::now();
                let guard = rc.try_get_exclusive_ref();
                total += start.elapsed();
                drop(black_box(guard));
            }
            total
        });
    });

    write.bench_function("tokio", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::new(0, 0);
            for _ in 0..iters {
                let start = std::time::Instant::now();
                let guard = tokio_rw_lock.try_write();
                total += start.elapsed();
                drop(black_box(guard));
            }
            total
        });
    });
    write.finish();

    unsafe { rc.unblock_refs() };
    let mut read = c.benchmark_group("get-read");

    read.bench_function("ref_count", |b| {
        b.iter(|| {
            let r = rc.try_get_ref();
            drop(black_box(r))
        })
    });

    read.bench_function("tokio", |b| {
        b.iter(|| {
            let guard = tokio_rw_lock.try_read();
            drop(guard)
        })
    });
    read.finish();

    // This needs to be optimized, right now the orientation is purely thread safety.
    c.bench_function("drop-exclusive", |b| {
        b.iter(|| {
            unsafe { rc.unblock_refs() };
        })
    });
}

criterion_group!(benches, get_ref);
criterion_main!(benches);