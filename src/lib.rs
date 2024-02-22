#![doc = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))]
#![cfg_attr(not(feature = "std"), no_std)]
#![no_builtins]
#![warn(
    clippy::all,
    clippy::nursery,
    clippy::pedantic,
    clippy::cargo,
)]
#![allow(
    clippy::cargo_common_metadata,
    clippy::module_name_repetitions,
    clippy::multiple_crate_versions
)]

#[cfg(feature = "alloc")]
extern crate alloc;

#[macro_use]
mod macros;
pub mod consts;
pub mod futures;
pub mod array_queue;
pub mod backoff;
pub mod pad;
pub mod waiter;

use core::fmt::{self, Formatter, Debug, Display};
pub(crate) use waiter::Waiter;
use array_queue::ArrayQueue;
use futures::{FutureRef, FutureExclusiveRef, FutureBlockedRefs};
use pad::CacheLine;
use backoff::Backoff;
use futures::MaybeFuture;

/// # Reference State
///
/// Denoting whether references can take place or are blocked.
///
/// # Variants
///
/// - `Blocked`: References must wait, there is currently an exclusive reference or a request for
///              one.
/// - `Allowed`: References can take place.
///
/// # Example
///
/// ```
/// use ref_count::{RefCount, State};
/// let ref_count = RefCount::<32>::new();
///
/// // state() returns this.
/// assert!(ref_count.state().is_allowed());
///
/// let exclusive = ref_count.try_get_exclusive_ref().unwrap();
/// assert!(ref_count.state().is_blocked());
/// ```
#[repr(u32)]
pub enum State {
    Blocked = 1,
    Allowed = 0
}

impl State {
    #[inline] #[must_use]
    pub const fn from_raw(state: u32) -> Self {
        // SAFETY:
        // With the mask, the value can only be 0 or 1, which are the two possible values for the
        // enums discriminant. The state and the enum are both represented as an unsigned 32-bit
        // integers. The usage of transmute here is documented in the block_state macro as well as
        // the mask constant to ensure modifications of these do not yield UB in the future.
        unsafe { core::mem::transmute(block_state!(state)) }
    }

    /// Returns true if references were allowed when the [`State`] was constructed.
    #[inline] #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// Returns true if references were blocked when the [`State`] was constructed.
    #[inline] #[must_use]
    pub const fn is_blocked(&self) -> bool {
        matches!(self, Self::Blocked)
    }
}

macro_rules! __state_format {
    ($state:ident, $formatter:ident) => {
        match $state {
            Self::Allowed => $formatter.write_str("allowed"),
            Self::Blocked => $formatter.write_str("blocked")
        }
    };
}

impl Display for State {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        __state_format!(self, f)
    }
}

impl Debug for State {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        __state_format!(self, f)
    }
}

/// # Maybe Await
///
/// A more ergonomic way to conditionally await a [`MaybeFuture`].
///
/// # Example
///
/// ```
/// use ref_count::{RefCount, maybe_await};
///
/// async fn example() {
///     let ref_count = RefCount::<32>::new();
///     let reference = maybe_await!(ref_count.get_ref());
///     # drop(reference)
/// }
/// ```
#[macro_export]
macro_rules! maybe_await {
    ($maybe_fut:expr) => {{
        match $maybe_fut {
            $crate::futures::MaybeFuture::Future(__fut) => __fut.await,
            $crate::futures::MaybeFuture::Ready(__res)  => __res
        }
    }};
}

/// # Reference Counter
///
/// A thread-safe reference counter with both standard and exclusive references.
///
/// # Basic Example
///
/// ```
/// use ref_count::{RefCount, maybe_await};
///
/// async fn example() {
///     let ref_count = RefCount::<32>::new();
///     let reference = maybe_await!(ref_count.get_ref());
///
///     // if we want an exclusive reference, ensure we are not holding a reference on the same
///     // thread, or we will cause a deadlock.
///     drop(reference);
///     let exclusive = maybe_await!(ref_count.get_exclusive_ref());
///     // now we know no other thread has a reference, and if requesting one is a future.
///
///     drop(exclusive);
///     // now other threads once again can get a reference.
/// }
/// ```
///
/// ## Fairness
///
/// Fairness is ensured up to the constant `MAX_WAITING`, as once the queue is full the ordering
/// of insertions to the queue can not be ensured. If you require complete fairness set this to a
/// large value, the size of each `Waiter` is 22 bytes. If your environment permits it, activating
/// the `alloc` feature could be a good decision if setting this high.
///
/// In general use one won't experience issues with this, the prevention of priority inversion and
/// starvation has been tested rigorously. Regardless, users should be aware of this.
pub struct RefCount<const MAX_WAITING: usize = 32> {
    count: CacheLine<atomic!(AtomicU32, ty)>,
    #[cfg(feature = "alloc")]
    waiter_queue: alloc::boxed::Box<ArrayQueue<Waiter, MAX_WAITING>>,
    #[cfg(not(feature = "alloc"))]
    waiter_queue: ArrayQueue<Waiter, MAX_WAITING>,
}

impl<const MAX_WAITING: usize> Default for RefCount<MAX_WAITING> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<const MAX_WAITING: usize> RefCount<MAX_WAITING> {
    #[cfg(not(feature = "alloc"))]
    #[must_use] pub fn new() -> Self {
        Self {
            count: CacheLine::new(atomic!(u32)),
            waiter_queue: ArrayQueue::new()
        }
    }

    #[cfg(feature = "alloc")]
    #[must_use] pub fn new() -> Self {
        Self {
            count: CacheLine::new(atomic!(u32)),
            waiter_queue: alloc::boxed::Box::new(ArrayQueue::new())
        }
    }

    /// # Reference Count
    ///
    /// Unlike [`standard_ref_count`] this returns the number of current references, regardless if
    /// that be standard or exclusive references.
    ///
    /// # Example
    ///
    /// ```
    /// # let ref_count = ref_count::RefCount::<32>::new();
    /// let reference = ref_count.try_get_ref().unwrap();
    /// assert_eq!(ref_count.ref_count(), 1);
    ///
    /// drop(reference);
    ///
    /// let exclusive = ref_count.try_get_exclusive_ref().unwrap();
    /// assert_eq!(ref_count.ref_count(), 1);
    /// ```
    ///
    /// [`standard_ref_count`]: RefCount::standard_ref_count
    #[inline] #[must_use]
    pub fn ref_count(&self) -> u32 {
        let state = self.count.load(ordering!(Acquire));
        standard_ref_count!(state) + block_state!(state)
    }

    /// # No References
    ///
    /// Returns true if there are not currently any references of any kind, just with a few cycles
    /// of overhead over [`no_standard_refs`]. False if there are any references of any kind.
    ///
    /// # Example
    ///
    /// ```
    /// # let ref_count = ref_count::RefCount::<32>::new();
    /// let reference = ref_count.try_get_ref().unwrap();
    /// assert!(!ref_count.no_refs());
    ///
    /// drop(reference);
    /// assert!(ref_count.no_refs());
    ///
    /// let exclusive = ref_count.try_get_exclusive_ref().unwrap();
    /// assert!(!ref_count.no_refs());
    /// ```
    ///
    /// [`no_standard_refs`]: RefCount::no_standard_refs
    #[inline] #[must_use]
    pub fn no_refs(&self) -> bool {
        self.ref_count() == 0
    }

    /// # Standard Reference Count
    ///
    /// This returns the current number of **standard** references, if there is currently an
    /// exclusive reference, this will return 0. If you are ok with a few cycles of overhead, but
    /// need the current reference count regardless of the type of reference see [`ref_count`].
    ///
    /// # Example
    ///
    /// ```
    /// # let ref_count = ref_count::RefCount::<32>::new();
    /// let reference = ref_count.try_get_ref().unwrap();
    /// assert_eq!(ref_count.standard_ref_count(), 1);
    ///
    /// drop(reference);
    ///
    /// let exclusive = ref_count.try_get_exclusive_ref().unwrap();
    /// assert_eq!(ref_count.standard_ref_count(), 0); // as doesn't include exclusive refs.
    /// ```
    ///
    /// [`ref_count`]: RefCount::ref_count
    #[inline] #[must_use]
    pub fn standard_ref_count(&self) -> u32 {
        standard_ref_count!(self.count.load(ordering!(Acquire)))
    }

    /// # No Standard References
    ///
    /// Returns true if there are not currently any standard references. Will also return true if
    /// there is an exclusive reference. If you need to know if there are no references of any
    /// kind, use [`no_refs`] instead.
    ///
    /// # Example
    ///
    /// ```
    /// # let ref_count = ref_count::RefCount::<32>::new();
    /// let reference = ref_count.try_get_ref().unwrap();
    /// assert!(!ref_count.no_standard_refs());
    ///
    /// drop(reference);
    /// assert!(ref_count.no_standard_refs());
    ///
    /// let exclusive = ref_count.try_get_exclusive_ref().unwrap();
    /// assert!(ref_count.no_standard_refs()); // as doesn't consider exclusive refs.
    /// assert!(!ref_count.no_refs()); // although this does.
    /// ```
    ///
    /// [`no_refs`]: RefCount::no_refs
    #[inline] #[must_use]
    pub fn no_standard_refs(&self) -> bool {
        self.standard_ref_count() == 0
    }

    /// # Raw Reference State
    ///
    /// Returns 1 if references are currently blocked, and 0 if they are allowed.
    ///
    /// # Example
    ///
    /// ```
    /// # use ref_count::{RefCount, State};
    /// # let ref_count = RefCount::<32>::new();
    /// assert_eq!(ref_count.raw_state(), 0);
    ///
    /// let exclusive = ref_count.try_get_exclusive_ref().unwrap();
    /// assert_eq!(ref_count.raw_state(), 1);
    /// ```
    #[inline] #[must_use]
    pub fn raw_state(&self) -> u32 {
        block_state!(self.count.load(ordering!(Acquire)))
    }

    /// # Is Blocked
    ///
    /// Returns true if the state is currently blocked, false otherwise.
    ///
    /// # Example
    ///
    /// ```
    /// # use ref_count::{RefCount, State};
    /// # let ref_count = RefCount::<32>::new();
    /// assert_eq!(ref_count.is_blocked(), false);
    ///
    /// let exclusive = ref_count.try_get_exclusive_ref().unwrap();
    /// assert_eq!(ref_count.is_blocked(), true);
    /// ```
    #[inline] #[must_use]
    pub fn is_blocked(&self) -> bool {
        self.raw_state() == 1
    }

    /// # Is Allowed
    ///
    /// Returns true if the state is currently blocked, false otherwise.
    ///
    /// # Example
    ///
    /// ```
    /// # use ref_count::{RefCount, State};
    /// # let ref_count = RefCount::<32>::new();
    /// assert_eq!(ref_count.is_allowed(), true);
    ///
    /// let exclusive = ref_count.try_get_exclusive_ref().unwrap();
    /// assert_eq!(ref_count.is_allowed(), false);
    /// ```
    #[inline] #[must_use]
    pub fn is_allowed(&self) -> bool {
        self.raw_state() == 0
    }

    /// # Reference State
    ///
    /// Returns the [`State`] enum, describing whether references are currently allowed or blocked.
    ///
    /// # Example
    ///
    /// ```
    /// # use ref_count::{RefCount, State};
    /// # let ref_count = RefCount::<32>::new();
    /// assert!(ref_count.state().is_allowed());
    ///
    /// let exclusive = ref_count.try_get_exclusive_ref().unwrap();
    /// assert!(ref_count.state().is_blocked());
    /// ```
    #[inline] #[must_use]
    pub fn state(&self) -> State {
        State::from_raw(self.raw_state())
    }

    /// # Try Block References
    ///
    /// If successful (denoted by it returning `true`) no references can take place until unblocked.
    /// To ensure that references are blocked on first invocation, see [`block_refs`]. It is best
    /// to just use the safe [`get_exclusive_ref`] or [`try_get_exclusive_ref`].
    ///
    /// # Safety
    ///
    /// It is the callers responsibility to ensure they [`unblock_refs`] if the invocation was
    /// successful to ensure they do not deadlock.
    ///
    /// # Examples
    ///
    /// Safe Usage
    /// ```
    /// # let ref_count = ref_count::RefCount::<32>::new();
    /// if unsafe { ref_count.try_block_refs() } {
    ///     if ref_count.no_standard_refs() {
    ///         // do something with mutual exclusion...
    ///     }
    /// }
    ///
    /// // now unblock references
    /// unsafe { ref_count.unblock_refs() };
    /// ```
    ///
    /// Unsafe Usage
    /// ```
    /// # let ref_count = ref_count::RefCount::<32>::new();
    /// unsafe { ref_count.try_block_refs() };
    /// // we are currently unsure if references are blocked, doing anything here, especially
    /// // unblocking refs will cause undefined behavior.
    /// ```
    ///
    /// [`block_refs`]: RefCount::block_refs
    /// [`get_exclusive_ref`]: RefCount::get_exclusive_ref
    /// [`try_get_exclusive_ref`]: RefCount::try_get_exclusive_ref
    /// [`unblock_refs`]: RefCount::unblock_refs
    #[inline]
    #[must_use = "Ignoring the output brings the opportunity for race conditions and deadlocks."]
    pub unsafe fn try_block_refs(&self) -> bool {
        // we first get the current state, and mask out the trailing bit to unlocked ensuring that
        // we cannot block refs if refs are already being blocked
        let mut ref_count = self.count.load(ordering!(Relaxed)) & state_const!(REF_COUNT_MASK);
        let mut next_ref_count = ref_count | state_const!(REF_BLOCKED);
        weak_try_update_ref_count!(block, self.count, ref_count, next_ref_count, {return true});
        weak_try_update_ref_count!(block, self.count, ref_count, next_ref_count, {return true});
        weak_try_update_ref_count!(block, self.count, ref_count, next_ref_count, {return true});
        weak_try_update_ref_count!(self.count, ref_count, next_ref_count, {return true}, |_s| {});

        false
    }

    /// # Block References
    ///
    /// No references can take place until unblocked after this has completed. It is best to
    /// use the safe alternatives for mutual exclusion: [`get_exclusive_ref`] or
    /// [`try_get_exclusive_ref`].
    ///
    /// # Safety
    ///
    /// It is the callers responsibility to [`unblock_refs`] after using to ensure they do not cause
    /// a deadlock.
    ///
    /// # Example
    ///
    /// ```
    /// # use ref_count::{RefCount, maybe_await};
    /// async fn example() {
    ///     let ref_count = RefCount::<32>::new();
    ///     maybe_await!(unsafe { ref_count.block_refs() });
    ///     // now that references are blocked, you do not know that you have mutual exclusion,
    ///     // you just know that no new references can take place.
    ///
    ///     if ref_count.no_standard_refs() {
    ///         // we have mutual exclusion here.
    ///     }
    ///     // we now unblock refs so we don't deadlock.
    ///     unsafe { ref_count.unblock_refs() };
    /// }
    /// ```
    ///
    /// # Returns
    ///
    /// [`MaybeFuture`], as under high contention this will resort to a future rather than spinning.
    ///
    /// [`get_exclusive_ref`]: RefCount::get_exclusive_ref
    /// [`try_get_exclusive_ref`]: RefCount::try_get_exclusive_ref
    /// [`unblock_refs`]: RefCount::unblock_refs
    #[inline]
    pub unsafe fn block_refs(&self) -> MaybeFuture<(), FutureBlockedRefs<MAX_WAITING>> {
        if !self.try_block_refs() {
            return MaybeFuture::Future(FutureBlockedRefs::new(self))
        }

        MaybeFuture::Ready(())
    }

    /// # Unblock References
    ///
    /// After blocking references on the thread, use this to now release this block and allow
    /// new references to be had. This will wake the queue of futures.
    ///
    /// # Safety
    ///
    /// The caller on the same thread should be the one currently blocking the references with
    /// [`try_block_refs`] or [`block_refs`], if this is not the case calling this will cause
    /// undefined behavior.
    ///
    /// # Examples
    ///
    /// Safe Usage
    /// ```
    /// # let ref_count = ref_count::RefCount::<32>::new();
    /// // block the references on the current thread
    /// if unsafe { ref_count.try_block_refs() } {
    ///     // do something like wait for standard references to drop to 0.
    ///
    ///     // now release the block safely
    ///     unsafe { ref_count.unblock_refs() };
    /// }
    /// ```
    ///
    /// Unsafe Usage
    /// ```
    /// # let ref_count = ref_count::RefCount::<32>::new();
    /// // this allows for race conditions
    /// unsafe { ref_count.unblock_refs() };
    ///
    /// let exclusive = ref_count.try_get_exclusive_ref().unwrap();
    /// // even though we are holding the block, it would be released twice, allowing
    /// // for race conditions again.
    /// unsafe { ref_count.unblock_refs() };
    /// ```
    ///
    /// [`try_block_refs`]: RefCount::try_block_refs
    /// [`block_refs`]: RefCount::block_refs
    #[inline]
    pub unsafe fn unblock_refs(&self) {
        self.count.store(state_const!(REF_ALLOWED), ordering!(Release));

        self.waiter_queue.pop()
            .map_or_else(|| slight_spin!(0), Waiter::wake);

        while let Some(waker) = self.waiter_queue.pop() {
            waker.wake();
        }
    }

    /// # Get an Exclusive Reference
    ///
    /// Returns an exclusive reference, while this reference is in scope no new references
    /// of any kind can be created.
    ///
    /// # Example
    ///
    /// ```
    /// # use ref_count::{RefCount, maybe_await};
    /// async fn example() {
    ///     let ref_count = RefCount::<32>::new();
    ///     assert!(ref_count.state().is_allowed());
    ///     let exclusive = maybe_await!(ref_count.get_exclusive_ref());
    ///     assert!(ref_count.state().is_blocked());
    ///     drop(exclusive);
    ///     assert!(ref_count.state().is_allowed());
    /// }
    /// ```
    ///
    /// # Deadlock
    ///
    /// If you are holding any other form of reference on the same thread and await for an exclusive
    /// reference it will attempt to get said reference until the termination of your program.
    ///
    /// # Deadlock Example
    ///
    /// ```
    /// # use ref_count::{RefCount, maybe_await};
    /// async fn example() {
    ///     let ref_count = RefCount::<32>::new();
    ///     let _first_exclusive = maybe_await!(ref_count.get_exclusive_ref());
    ///
    ///     let _deadlock = maybe_await!(ref_count.get_exclusive_ref());
    /// }
    /// ```
    pub fn get_exclusive_ref(&self) -> MaybeFuture<ExclusiveRef<MAX_WAITING>, FutureExclusiveRef<MAX_WAITING>> {
        if unsafe { self.try_block_refs() } {
            if self.no_standard_refs() {
                MaybeFuture::Ready(unsafe { ExclusiveRef::new(self) })
            } else {
                MaybeFuture::Future(FutureExclusiveRef::new(
                    self,
                    None
                ))
            }
        } else {
            MaybeFuture::Future(FutureExclusiveRef::new(
                self,
                Some(FutureBlockedRefs::new(self))
            ))
        }
    }

    /// # Try to Get an Exclusive Reference
    ///
    /// Attempt to get an exclusive reference, if there is high contention or references lasting
    /// the duration of this invocation this will return `None`.
    ///
    /// # Example
    ///
    /// ```
    /// # use ref_count::RefCount;
    /// let ref_count = RefCount::<32>::new();
    /// assert!(ref_count.state().is_allowed());
    ///
    /// let exclusive = ref_count.try_get_exclusive_ref().unwrap();
    /// assert!(ref_count.state().is_blocked());
    ///
    /// assert!(ref_count.try_get_ref().is_none());
    /// assert!(ref_count.try_get_exclusive_ref().is_none());
    ///
    /// drop(exclusive);
    /// assert!(ref_count.state().is_allowed());
    ///
    /// let _a_ref = ref_count.try_get_ref().unwrap();
    /// assert!(ref_count.try_get_exclusive_ref().is_none());
    /// ```
    pub fn try_get_exclusive_ref(&self) -> Option<ExclusiveRef<MAX_WAITING>> {
        if unsafe { self.try_block_refs() } {
            for i in 0..5 {
                if self.no_standard_refs() {
                    return Some(unsafe { ExclusiveRef::new(self) })
                }
                slight_spin!(i);
            }

            // there were still active references, release the block
            unsafe { self.unblock_refs() };
        }
        None
    }

    /// # Blocking Get Exclusive Reference
    ///
    /// This will spin until acquisition, defeating the purpose of this crate, the inclusion of this
    /// in the api is mostly for ease of testing / flexibility. This is wasteful, it will contribute
    /// to thrashing, and overall is just a bad practice to use in production environments.
    ///
    /// # Deadlocks
    ///
    /// Like [`get_exclusive_ref`] this can deadlock if used improperly. If you are currently
    /// holding any form of reference to the `RefCount` and invoke this, as that lock will not be
    /// released until end of scope, it will never be released as this will continue spinning until
    /// that reference is released.
    ///
    /// # Example
    ///
    /// ```
    /// # use ref_count::RefCount;
    /// let ref_count = RefCount::<32>::new();
    ///
    /// let exclusive = ref_count.blocking_get_exclusive_ref();
    /// assert!(ref_count.is_blocked());
    /// // do something with mutual exclusivity
    /// drop(exclusive);
    /// ```
    ///
    /// Deadlock
    /// ```
    /// # use ref_count::RefCount;
    /// let ref_count = RefCount::<32>::new();
    ///
    /// let first = ref_count.blocking_get_exclusive_ref();
    ///
    /// // we are holding an exclusive reference now, attempting to get another will cause this
    /// // thread to spin until termination of the program. As the example should be able to run,
    /// // calling this would make the test deadlock.
    /// // `let deadlock = ref_count.blocking_get_exclusive_ref();`
    ///
    /// let failure = ref_count.try_get_exclusive_ref();
    /// assert!(failure.is_none());
    /// # drop(first);
    ///
    /// // also works with standard refs of course.
    /// let first = ref_count.blocking_get_ref();
    ///
    /// let failure = ref_count.try_get_exclusive_ref();
    /// assert!(failure.is_none());
    ///
    /// # drop(first);
    /// ```
    ///
    /// [`get_exclusive_ref`]: RefCount::get_exclusive_ref
    pub fn blocking_get_exclusive_ref(&self) -> ExclusiveRef<MAX_WAITING> {
        let backoff = Backoff::new();

        while unsafe { !self.try_block_refs() } {
            backoff.snooze();
        }

        // now that references are blocked, wait for the number of references to reach 0.
        while !self.no_standard_refs() {
            backoff.snooze();
        }

        // now we know we have mutual exclusivity.
        unsafe { ExclusiveRef::new(self) }
    }

    /// # Get a Standard Reference
    ///
    /// Returns a standard reference, incrementing the count of active references. This method
    /// ensures that a reference is only obtained if it does not violate the conditions for
    /// exclusive access.
    ///
    /// # Deadlocks
    ///
    /// If you are currently holding an exclusive reference in the same scope, this will never be
    /// able to complete as that lock will never be released.
    ///
    /// # Example
    ///
    /// ```
    /// # use ref_count::{RefCount, maybe_await};
    /// async fn example() {
    ///     let ref_count = RefCount::<32>::new();
    ///     let standard_ref = maybe_await!(ref_count.get_ref());
    ///
    ///     assert_eq!(ref_count.ref_count(), 1);
    ///     drop(standard_ref);
    ///     assert!(ref_count.no_refs());
    /// }
    /// ```
    ///
    /// Deadlock Example
    /// ```
    /// # use ref_count::{RefCount, maybe_await};
    /// async fn example() {
    ///     let ref_count = RefCount::<32>::new();
    ///     let exclusive = maybe_await!(ref_count.get_exclusive_ref());
    ///
    ///     // now we attempt to acquire a reference, although it will never be able to due to us
    ///     // holding an exclusive reference.
    ///     let deadlock = maybe_await!(ref_count.get_ref());
    ///     // and it won't just deadlock this thread, the RefCount is locked right now for
    ///     // everyone, and that will not change until termination of the program.
    /// }
    /// ```
    ///
    /// # Contention Handling
    ///
    /// Under high contention or when exclusive access is requested, this method will resort to
    /// returning a future, deferring acquisition until it can safely proceed.
    ///
    /// ```
    /// # use ref_count::{RefCount, maybe_await};
    /// async fn high_contention_example() {
    ///     let ref_count = RefCount::<32>::new();
    ///     // Imagine another part of the application currently holds or is waiting for exclusive access.
    ///     let standard_ref = maybe_await!(ref_count.get_ref());
    ///     // This will only succeed once the exclusive access is resolved.
    /// }
    /// ```
    ///
    /// # Returns
    ///
    /// A `MaybeFuture<Ref, FutureRef>`, which can be immediately ready with a `Ref` or a future
    /// that resolves to a `Ref` when the system can safely grant access.
    #[inline]
    pub fn get_ref(&self) -> MaybeFuture<Ref<MAX_WAITING>, FutureRef<MAX_WAITING>> {
        // We use ref count mask here, as this will be the expected state in our CAS. Therefore, if
        // locked it will fail, but also can fail under high contention, although due to the design
        // regardless of the outcome, correctness will not be sacrificed.
        let mut ref_count = self.count.load(ordering!(Relaxed)) & state_const!(REF_COUNT_MASK);
        let mut next_ref_count = ref_count + state_const!(REF_INCR);

        many_weak_try_incr_refs!(self.count, ref_count, next_ref_count, {
            return MaybeFuture::Ready(unsafe { Ref::new(self) })
        });

        // ok, we weren't able to increment, resort to the future.
        return MaybeFuture::Future(FutureRef::new(self))
    }

    /// # Try to Get a Standard Reference
    ///
    /// Attempts to obtain a standard reference without blocking. This method will fail and return
    /// `None` if acquiring the reference would violate exclusive access conditions or if the system
    /// is under high contention.
    ///
    /// # Example
    ///
    /// ```
    /// # use ref_count::RefCount;
    /// let ref_count = RefCount::<32>::new();
    /// match ref_count.try_get_ref() {
    ///     Some(standard_ref) => {
    ///         assert_eq!(ref_count.ref_count(), 1);
    ///         drop(standard_ref);
    ///     },
    ///     None => {
    ///         // Could not acquire reference due to exclusive access or contention.
    ///     }
    /// }
    /// assert!(ref_count.no_refs());
    /// ```
    ///
    /// # Use Case
    ///
    /// This method is ideal for scenarios where failing fast is preferred over waiting for a
    /// reference to become available. It ensures that the system can proceed without delay, even
    /// if it means not acquiring the reference.
    ///
    /// # Returns
    ///
    /// An `Option<Ref>`, where `Some(Ref)` indicates a successful acquisition of the reference, and
    /// `None` indicates failure to acquire due to contention or exclusive access being granted.
    #[inline]
    pub fn try_get_ref(&self) -> Option<Ref<MAX_WAITING>> {
        let mut ref_count = self.count.load(ordering!(Relaxed)) & state_const!(REF_COUNT_MASK);
        let mut next_ref_count = ref_count + state_const!(REF_INCR);

        many_weak_try_incr_refs!(self.count, ref_count, next_ref_count, {
            return Some(unsafe { Ref::new(self) })
        });

        None
    }

    /// # Blocking Get Reference
    ///
    /// This will spin until acquisition of the reference, it is bad practice to use this in
    /// production environments, and it defeats the purpose of this crate.
    ///
    /// # Deadlocks
    ///
    /// The same as [`get_ref`], this will deadlock if you are currently holding an exclusive
    /// reference in scope.
    ///
    /// # Example
    ///
    /// ```
    /// # use ref_count::RefCount;
    /// let ref_count = RefCount::<32>::new();
    ///
    /// let reference = ref_count.blocking_get_ref();
    /// // read or something knowing no exclusive references can be created.
    /// assert_eq!(ref_count.ref_count(), 1);
    /// assert!(ref_count.is_allowed());
    /// # drop(reference)
    /// ```
    ///
    /// Deadlock Example
    /// ```
    /// # use ref_count::RefCount;
    /// let ref_count = RefCount::<32>::new();
    ///
    /// let reference = ref_count.blocking_get_ref();
    ///
    /// // now that we are holding a reference, an exclusive reference cannot be created
    /// // so by requesting one while holding this reference we will cause a deadlock.
    /// // let exclusive = ref_count.blocking_get_exclusive_ref(); // this would deadlock the test
    /// let attempt = ref_count.try_get_exclusive_ref();
    /// assert!(attempt.is_none());
    /// # drop(reference);
    /// ```
    ///
    /// [`get_ref`]: RefCount::get_ref
    pub fn blocking_get_ref(&self) -> Ref<MAX_WAITING> {
        let backoff = Backoff::new();
        let mut ref_count = self.count.load(ordering!(Relaxed)) & state_const!(REF_COUNT_MASK);
        let mut next_ref_count = ref_count + state_const!(REF_INCR);

        loop {
            many_weak_try_incr_refs!(self.count, ref_count, next_ref_count, {
                return unsafe { Ref::new(self) }
            });
            backoff.snooze();
        }
    }
}

impl<const MAX_WAITING: usize> Debug for RefCount<MAX_WAITING> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let state = self.count.load(ordering!(Relaxed));
        f.debug_struct("RefCount")
            .field("state", &State::from_raw(state))
            .field("references", &(standard_ref_count!(state) + block_state!(state)))
            .field("waiter_queue", &self.waiter_queue)
            .finish()
    }
}

/// # Standard Reference
///
/// This is a non-exclusive reference to a `RefCount`, many of these can exist at once.
///
/// # Example
///
/// ```
/// use ref_count::{RefCount, Ref, maybe_await};
///
/// async fn example() {
///     let ref_count = RefCount::<32>::new();
///
///     let r: Ref<'_, 32> = maybe_await!(ref_count.get_ref());
///     // do something with assurance no exclusive reference can exist...
///     drop(r);
/// }
/// ```
///
/// # Returned By
///
/// - [`get_ref`]
/// - [`try_get_ref`]
/// - [`blocking_get_ref`]
///
/// [`get_ref`]: RefCount::get_ref
/// [`try_get_ref`]: RefCount::try_get_ref
/// [`blocking_get_ref`]: RefCount::blocking_get_ref
#[repr(transparent)]
pub struct Ref<'count, const MAX_WAITING: usize> {
    src: &'count RefCount<MAX_WAITING>
}

impl<'count, const MAX_WAITING: usize> Ref<'count, MAX_WAITING> {
    /// # Safety
    ///
    /// This does not increment the counter, but will decrement it when dropped. The caller
    /// must increment the counter prior to invoking.
    #[inline]
    pub const unsafe fn new(counter: &'count RefCount<MAX_WAITING>) -> Self {
        Self {
            src: counter
        }
    }
}

impl<'count, const MAX_WAITING: usize> Drop for Ref<'count, MAX_WAITING> {
    /// Decrements the reference count.
    #[inline]
    fn drop(&mut self) {
        self.src.count.fetch_sub(state_const!(REF_INCR), ordering!(Release));
    }
}

/// # Exclusive Reference
///
/// When an exclusive reference is in scope, no other references to the `RefCount` are able to
/// exist.
///
/// # Example
///
/// ```
/// use ref_count::{RefCount, ExclusiveRef, maybe_await};
///
/// async fn example() {
///     let ref_count = RefCount::<32>::new();
///
///     let e: ExclusiveRef<'_, 32> = maybe_await!(ref_count.get_exclusive_ref());
///     // Do something with the assurance of mutual exclusion...
///     drop(e);
/// }
/// ```
///
/// # Returned By
///
/// - [`get_exclusive_ref`]
/// - [`try_get_exclusive_ref`]
/// - [`blocking_get_exclusive_ref`]
///
/// [`get_exclusive_ref`]: RefCount::get_exclusive_ref
/// [`try_get_exclusive_ref`]: RefCount::try_get_exclusive_ref
/// [`blocking_get_exclusive_ref`]: RefCount::blocking_get_exclusive_ref
#[repr(transparent)]
pub struct ExclusiveRef<'count, const MAX_WAITING: usize> {
    src: &'count RefCount<MAX_WAITING>
}

impl<'count, const MAX_WAITING: usize> ExclusiveRef<'count, MAX_WAITING> {
    /// # Safety
    ///
    /// This merely creates an exclusive reference, not ensuring exclusivity. Creating this with
    /// ignorance to the fundamental properties of `ExclusiveRef` will yield undefined behavior.
    #[inline]
    pub const unsafe fn new(ref_count: &'count RefCount<MAX_WAITING>) -> Self {
        Self { src: ref_count }
    }
}

impl<'count, const MAX_WAITING: usize> Drop for ExclusiveRef<'count, MAX_WAITING> {
    /// Safely invokes [`unblock_refs`].
    ///
    /// [`unblock_refs`]: RefCount::unblock_refs
    #[inline]
    fn drop(&mut self) {
        unsafe {
            self.src.unblock_refs();
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{MaybeFuture, RefCount, State};
    use core::task::{Poll, RawWaker, RawWakerVTable, Context, Waker};
    use core::pin::pin;
    use core::future::Future;

    static VTABLE: RawWakerVTable = RawWakerVTable::new(
        |_ptr| RawWaker::new(&() as *const _ as *const (), &VTABLE),
        |_ptr| (),
        |_ptr| (),
        |_ptr| (),
    );

    macro_rules! poll {
        ($pinned_fut:expr) => {{
            $pinned_fut.as_mut().poll(&mut Context::from_waker(
                &unsafe { Waker::from_raw(RawWaker::new(&() as *const _ as *const (), &VTABLE))}
            ))
        }};
    }

    #[test]
    fn exclusive_vs_ref() {
        let ref_counter = RefCount::<30>::new();
        let r = match ref_counter.get_ref() {
            MaybeFuture::Ready(r) => r,
            MaybeFuture::Future(_) => panic!("Went to future without contention")
        };

        let deadlock_fut = match ref_counter.get_exclusive_ref() {
            MaybeFuture::Ready(_) => panic!("should have deadlocked but was ready"),
            MaybeFuture::Future(fut) => fut
        };

        let not_yet = match ref_counter.get_ref() {
            MaybeFuture::Ready(_) => panic!("Got new ref after exclusive ref made request"),
            MaybeFuture::Future(f) => f,
        };

        let mut not_yet_fut = pin!(not_yet);
        let mut fut = pin!(deadlock_fut);

        for _ in 0..5000 {
            assert!(matches!(poll!(fut), Poll::Pending));
        }

        drop(r);

        // now should be ready
        assert!(matches!(poll!(fut), Poll::Ready(_)));
        assert!(matches!(poll!(not_yet_fut), Poll::Ready(_)));
    }

    #[test]
    fn state_enum_from_raw() {
        let e = State::from_raw(1);
        assert!(e.is_blocked());

        let e = State::from_raw(0);
        assert!(e.is_allowed());
    }
}