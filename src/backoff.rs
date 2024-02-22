//! Simple Backoff

use core::cell::Cell;

const YIELD_POINT: usize = 64;

#[repr(transparent)]
pub struct Backoff {
    calls: Cell<usize>
}

#[cfg(loom)]
macro_rules! yield_now {
    () => {
        ::loom::thread::yield_now()
    };
}

#[cfg(all(not(loom), feature = "std"))]
macro_rules! yield_now {
    () => {
        ::std::thread::yield_now()
    };
}

#[cfg(not(any(loom, feature = "std")))]
macro_rules! yield_now {
    () => {};
}

impl Backoff {
    #[inline] #[must_use]
    pub const fn new() -> Self {
        Self { calls: Cell::new(0) }
    }

    pub fn spin(&self) {
        let calls = self.calls.get();
        slight_spin!(calls);
        self.calls.set(calls + 1);
    }

    #[inline]
    pub fn reset(&self) {
        self.calls.set(0);
    }

    pub fn snooze(&self) {
        let calls = self.calls.get();
        slight_spin!(calls);
        if calls < YIELD_POINT {
            self.calls.set(calls + 1);
        } else {
            yield_now!();
            self.reset();
        }
    }
}