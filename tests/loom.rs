#[cfg(all(loom, test))]
mod loom {
    use ref_count::{RefCount, Ref, maybe_await};
    use loom::thread;
    use core::ops::Deref;
    use loom::sync::Arc;
    use loom::future::block_on;
    use loom::cell::{UnsafeCell, ConstPtr};

    const W: usize = 32;

    struct RwLock<T> {
        data: UnsafeCell<T>,
        state: RefCount<W>
    }

    struct ReadGuard<'lock, T> {
        _r: Ref<'lock, W>,
        data: ConstPtr<T>
    }

    impl<'lock, T> Deref for ReadGuard<'lock, T> {
        type Target = T;

        fn deref(&self) -> &Self::Target {
            unsafe { self.data.deref() }
        }
    }

    impl<T> RwLock<T> {
        pub fn new(data: T) -> Self {
            Self {
                data: UnsafeCell::new(data),
                state: RefCount::new()
            }
        }

        pub fn try_read(&self) -> Option<ReadGuard<T>> {
            let reference = self.state.try_get_ref()?;
            Some(ReadGuard {
                data: self.data.get(),
                _r: reference
            })
        }

        pub async fn write_to(&self, val: T) {
            let exclusive = maybe_await!(self.state.get_exclusive_ref());
            self.data.with_mut(|f| unsafe {f.write(val)});
            drop(exclusive);
        }

        pub fn try_write_to(&self, val: T) -> Option<()> {
            let exclusive = self.state.try_get_exclusive_ref()?;
            self.data.with_mut(|f| unsafe {f.write(val)});
            drop(exclusive);
            Some(())
        }
    }

    #[test]
    fn writers() {
        loom::model(|| {
            let rwlock = Arc::new(RwLock::new(0));
            let rw = rwlock.clone();
            let t1 = thread::spawn(move || {
                block_on(async {
                    rw.write_to(5).await;
                });
            });

            let rw = rwlock.clone();
            let t2 = thread::spawn(move || {
                block_on(async {
                    rw.write_to(5).await;
                });
            });

            t1.join().unwrap();
            t2.join().unwrap();
        });
    }

    #[test]
    fn readers() {
        loom::model(|| {
            let rwlock = Arc::new(RwLock::new(0));
            let rw = rwlock.clone();
            let t1 = thread::spawn(move || {
                let read = rw.try_read();
                drop(read);
            });

            let rw = rwlock.clone();
            let t2 = thread::spawn(move || {
                let read = rw.try_read();
                drop(read);
            });

            t1.join().unwrap();
            t2.join().unwrap();
        })
    }

    #[test]
    fn reader_writer() {
        loom::model(|| {
            let rwlock = Arc::new(RwLock::new(0));
            let rw = rwlock.clone();
            let t1 = thread::spawn(move || {
                rw.try_write_to(5);
            });

            let rw = rwlock.clone();
            let t2 = thread::spawn(move || {
                let read = rw.try_read();
                drop(read);
            });

            t1.join().unwrap();
            t2.join().unwrap();
        })
    }
}
