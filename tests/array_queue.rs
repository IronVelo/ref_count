use ref_count::array_queue::ArrayQueue;
use crossbeam_utils::thread::scope;
use core::sync::atomic::{AtomicUsize, Ordering};
use rand::{thread_rng, Rng};

#[test]
fn smoke() {
    let q = ArrayQueue::<i32, 1>::new();

    q.push(7).unwrap();
    assert_eq!(q.pop(), Some(7));

    q.push(8).unwrap();
    assert_eq!(q.pop(), Some(8));
    assert!(q.pop().is_none());
}

#[test]
#[should_panic(expected = "capacity must be non-zero")]
fn zero_capacity() {
    let _ = ArrayQueue::<i32, 0>::new();
}

#[test]
fn len_empty_full() {
    let mut x = AtomicUsize::new(5);
    x.get_mut();
    let q = ArrayQueue::<(), 2>::new();

    assert_eq!(q.len(), 0);
    assert!(q.is_empty());
    assert!(!q.is_full());

    q.push(()).unwrap();

    assert_eq!(q.len(), 1);
    assert!(!q.is_empty());
    assert!(!q.is_full());

    q.push(()).unwrap();

    assert_eq!(q.len(), 2);
    assert!(!q.is_empty());
    assert!(q.is_full());

    q.pop().unwrap();

    assert_eq!(q.len(), 1);
    assert!(!q.is_empty());
    assert!(!q.is_full());
}

#[test]
fn len() {
    #[cfg(miri)]
    const COUNT: usize = 30;
    #[cfg(not(miri))]
    const COUNT: usize = 25_000;
    #[cfg(miri)]
    const CAP: usize = 40;
    #[cfg(not(miri))]
    const CAP: usize = 1000;
    const ITERS: usize = CAP / 20;

    let q = ArrayQueue::<usize, CAP>::new();
    assert_eq!(q.len(), 0);

    for _ in 0..CAP / 10 {
        for i in 0..ITERS {
            q.push(i).unwrap();
            assert_eq!(q.len(), i + 1);
        }

        for i in 0..ITERS {
            q.pop().unwrap();
            assert_eq!(q.len(), ITERS - i - 1);
        }
    }
    assert_eq!(q.len(), 0);

    for i in 0..CAP {
        q.push(i).unwrap();
        assert_eq!(q.len(), i + 1);
    }

    for _ in 0..CAP {
        q.pop().unwrap();
    }
    assert_eq!(q.len(), 0);

    scope(|scope| {
        scope.spawn(|_| {
            for i in 0..COUNT {
                loop {
                    if let Some(x) = q.pop() {
                        assert_eq!(x, i);
                        break;
                    }
                }
                let len = q.len();
                assert!(len <= CAP);
            }
        });

        scope.spawn(|_| {
            for i in 0..COUNT {
                while q.push(i).is_err() {}
                let len = q.len();
                assert!(len <= CAP);
            }
        });
    })
        .unwrap();
    assert_eq!(q.len(), 0);
}

#[test]
fn spsc() {
    #[cfg(miri)]
    const COUNT: usize = 50;
    #[cfg(not(miri))]
    const COUNT: usize = 100_000;

    let q = ArrayQueue::<usize, 3>::new();

    scope(|scope| {
        scope.spawn(|_| {
            for i in 0..COUNT {
                loop {
                    if let Some(x) = q.pop() {
                        assert_eq!(x, i);
                        break;
                    }
                }
            }
            assert!(q.pop().is_none());
        });

        scope.spawn(|_| {
            for i in 0..COUNT {
                while q.push(i).is_err() {}
            }
        });
    })
        .unwrap();
}

#[test]
fn spsc_ring_buffer() {
    #[cfg(miri)]
    const COUNT: usize = 50;
    #[cfg(not(miri))]
    const COUNT: usize = 100_000;

    let t = AtomicUsize::new(1);
    let q = ArrayQueue::<usize, 3>::new();
    let v = (0..COUNT).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>();

    scope(|scope| {
        scope.spawn(|_| loop {
            match t.load(Ordering::SeqCst) {
                0 if q.is_empty() => break,

                _ => {
                    while let Some(n) = q.pop() {
                        v[n].fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        });

        scope.spawn(|_| {
            for i in 0..COUNT {
                if let Some(n) = q.force_push(i) {
                    v[n].fetch_add(1, Ordering::SeqCst);
                }
            }

            t.fetch_sub(1, Ordering::SeqCst);
        });
    })
        .unwrap();

    for c in v {
        assert_eq!(c.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn mpmc() {
    #[cfg(miri)]
    const COUNT: usize = 50;
    #[cfg(not(miri))]
    const COUNT: usize = 25_000;
    const THREADS: usize = 4;

    let q = ArrayQueue::<usize, 3>::new();
    let v = (0..COUNT).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>();

    scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|_| {
                for _ in 0..COUNT {
                    let n = loop {
                        if let Some(x) = q.pop() {
                            break x;
                        }
                    };
                    v[n].fetch_add(1, Ordering::SeqCst);
                }
            });
        }
        for _ in 0..THREADS {
            scope.spawn(|_| {
                for i in 0..COUNT {
                    while q.push(i).is_err() {}
                }
            });
        }
    })
        .unwrap();

    for c in v {
        assert_eq!(c.load(Ordering::SeqCst), THREADS);
    }
}

#[test]
fn mpmc_ring_buffer() {
    #[cfg(miri)]
    const COUNT: usize = 50;
    #[cfg(not(miri))]
    const COUNT: usize = 25_000;
    const THREADS: usize = 4;

    let t = AtomicUsize::new(THREADS);
    let q = ArrayQueue::<usize, 3>::new();
    let v = (0..COUNT).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>();

    scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|_| loop {
                match t.load(Ordering::SeqCst) {
                    0 if q.is_empty() => break,

                    _ => {
                        while let Some(n) = q.pop() {
                            v[n].fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
            });
        }

        for _ in 0..THREADS {
            scope.spawn(|_| {
                for i in 0..COUNT {
                    if let Some(n) = q.force_push(i) {
                        v[n].fetch_add(1, Ordering::SeqCst);
                    }
                }

                t.fetch_sub(1, Ordering::SeqCst);
            });
        }
    })
        .unwrap();

    for c in v {
        assert_eq!(c.load(Ordering::SeqCst), THREADS);
    }
}

#[test]
fn drops() {
    let runs: usize = if cfg!(miri) { 3 } else { 100 };
    let steps: usize = if cfg!(miri) { 50 } else { 10_000 };
    let additional: usize = if cfg!(miri) { 10 } else { 50 };

    static DROPS: AtomicUsize = AtomicUsize::new(0);

    #[derive(Debug, PartialEq)]
    struct DropCounter;

    impl Drop for DropCounter {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }

    let mut rng = thread_rng();

    for _ in 0..runs {
        let steps = rng.gen_range(0..steps);
        let additional = rng.gen_range(0..additional);

        DROPS.store(0, Ordering::SeqCst);
        let q = ArrayQueue::<DropCounter, 50>::new();

        scope(|scope| {
            scope.spawn(|_| {
                for _ in 0..steps {
                    while q.pop().is_none() {}
                }
            });

            scope.spawn(|_| {
                for _ in 0..steps {
                    while q.push(DropCounter).is_err() {
                        DROPS.fetch_sub(1, Ordering::SeqCst);
                    }
                }
            });
        })
            .unwrap();

        for _ in 0..additional {
            q.push(DropCounter).unwrap();
        }

        assert_eq!(DROPS.load(Ordering::SeqCst), steps);
        drop(q);
        assert_eq!(DROPS.load(Ordering::SeqCst), steps + additional);
    }
}

#[test]
fn linearizable() {
    #[cfg(miri)]
    const COUNT: usize = 100;
    #[cfg(not(miri))]
    const COUNT: usize = 25_000;
    const THREADS: usize = 4;

    let q = ArrayQueue::<i32, THREADS>::new();

    scope(|scope| {
        for _ in 0..THREADS / 2 {
            scope.spawn(|_| {
                for _ in 0..COUNT {
                    while q.push(0).is_err() {}
                    q.pop().unwrap();
                }
            });

            scope.spawn(|_| {
                for _ in 0..COUNT {
                    if q.force_push(0).is_none() {
                        q.pop().unwrap();
                    }
                }
            });
        }
    }).unwrap();
}

#[test]
fn into_iter() {
    let q = ArrayQueue::<usize, 100>::new();
    for i in 0..100 {
        q.push(i).unwrap();
    }
    for (i, j) in q.into_iter().enumerate() {
        assert_eq!(i, j);
    }
}