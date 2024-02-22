//! Wrapper for `Waker`
use core::task::Waker;

/// # Waiter
///
/// A waiter consists of a core `Waker` and a flag describing if the future has been registered.
/// On each invocation of the `wake` method the `registered` flag is set to false, communicating
/// to the future if it wasn't ready after being polled to re-register with the waiter queue.
pub struct Waiter {
    /// The core waiter for the future
    waker: Waker,
    /// A mutable pointer telling the future if it is currently registered in the queue
    registered: *mut bool
}

impl Waiter {
    #[inline]
    pub fn new(waker: Waker, registered: &mut bool) -> Self {
        Self {
            waker,
            registered: registered as *mut bool
        }
    }

    /// # Set Registered
    ///
    /// Update the futures registered flag, which the future should be able to access on next
    /// poll.
    pub fn set_registered(&self, is_registered: bool) {
        if !self.registered.is_null() {
            unsafe { self.registered.write(is_registered) };
        }
    }

    /// # Wake Future
    ///
    /// Wakes up the future, and sets the registered flag to `false`
    #[inline]
    pub fn wake(self) {
        self.set_registered(false);
        self.waker.wake();
    }

    /// # Inner Wake
    ///
    /// Wakes up the future without setting `registered` to false.
    #[inline]
    pub fn inner_wake(self) {
        self.waker.wake();
    }
}

unsafe impl Send for Waiter {}
unsafe impl Sync for Waiter {}