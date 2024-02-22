//! Queue for Polling Waiters

use core::cell::UnsafeCell;
use core::fmt;
use core::mem::{self, MaybeUninit};
use core::panic::{RefUnwindSafe, UnwindSafe};
use crate::backoff::Backoff;
use crate::pad::CacheLine;

#[cfg(loom)]
mod loom_extra {
    pub struct AtomicUsizeMutRef<'src> {
        src: &'src loom::sync::atomic::AtomicUsize,
        value: usize
    }

    impl<'src> core::ops::Deref for AtomicUsizeMutRef<'src> {
        type Target = usize;

        fn deref(&self) -> &Self::Target {
            &self.value
        }
    }

    impl<'src> core::ops::DerefMut for AtomicUsizeMutRef<'src> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.value
        }
    }

    impl<'src> Drop for AtomicUsizeMutRef<'src> {
        fn drop(&mut self) {
            self.src.store(self.value, loom::sync::atomic::Ordering::Relaxed);
        }
    }

    impl<'src> PartialEq for AtomicUsizeMutRef<'src> {
        fn eq(&self, other: &Self) -> bool {
            self.value == other.value
        }
    }

    pub trait AtomicExtras<'src> {
        type Output;

        fn get_mut(&'src self) -> Self::Output;
    }

    impl<'src> AtomicExtras<'src> for loom::sync::atomic::AtomicUsize {
        type Output = AtomicUsizeMutRef<'src>;

        fn get_mut(&'src self) -> Self::Output {
            AtomicUsizeMutRef {
                src: self,
                value: self.load(loom::sync::atomic::Ordering::Relaxed)
            }
        }
    }
}

#[cfg(loom)]
use loom_extra::AtomicExtras;

// in the future, attempt to implement for the loom UnsafeCell api for further verification.
// although this is most likely correct.
pub trait UnsafeCellExtras<T> {
    /// # Safety
    ///
    /// Voids sync. See [`UnsafeCell`] and [`core::ptr::write`] safety section.
    unsafe fn write(&self, d: T);
    /// # Safety
    ///
    /// Voids sync. See [`UnsafeCell`] and [`core::ptr::replace`] safety section.
    unsafe fn replace(&self, n: T) -> T;
    /// # Safety
    ///
    /// See [`core::ptr::read`] safety section.
    unsafe fn read(&self) -> T;
}

impl<T> UnsafeCellExtras<T> for UnsafeCell<T> {
    #[inline]
    unsafe fn write(&self, d: T) {
        self.get().write(d);
    }

    #[inline]
    unsafe fn replace(&self, n: T) -> T {
        self.get().replace(n)
    }

    #[inline]
    unsafe fn read(&self) -> T {
        self.get().read()
    }
}

macro_rules! queue_size {
    ($capacity:ident, $head:ident, $head_idx:ident, $tail:ident, $tail_idx:ident) => {
        if $head_idx < $tail_idx {
            $tail_idx - $head_idx
        } else if $head_idx > $tail_idx {
            $capacity - $head_idx + $tail_idx
        } else if $tail == $head {
            0
        } else {
            $capacity
        }
    };
}

/// A slot in a queue.
struct Slot<T> {
    /// The current stamp.
    ///
    /// If the stamp equals the tail, this node will be next written to. If it equals head + 1,
    /// this node will be next read from.
    stamp: atomic!(AtomicUsize, ty),

    /// The value in this slot.
    value: UnsafeCell<MaybeUninit<T>>,
}

/// A bounded multi-producer multi-consumer queue.
///
/// This queue allocates a fixed-capacity buffer on construction, which is used to store pushed
/// elements. The queue cannot hold more elements than the buffer allows. Attempting to push an
/// element into a full queue will fail. Alternatively, [`force_push`] makes it possible for
/// this queue to be used as a ring-buffer.
///
/// [`force_push`]: ArrayQueue::force_push
///
/// # Examples
///
/// ```
/// use ref_count::array_queue::ArrayQueue;
///
/// let q = ArrayQueue::<char, 2>::new();
///
/// assert_eq!(q.push('a'), Ok(()));
/// assert_eq!(q.push('b'), Ok(()));
/// assert_eq!(q.push('c'), Err('c'));
/// assert_eq!(q.pop(), Some('a'));
/// ```
///
/// ### Note
///
/// This is a port of the [`crossbeam-queue`] crate's `ArrayQueue`. Modifications were minimal,
/// the core of the changes, and the reason behind the port, was getting rid of the heap allocation.
///
/// [`crossbeam-queue`]: https://docs.rs/crossbeam-queue/latest/crossbeam_queue/struct.ArrayQueue.html
pub struct ArrayQueue<T, const C: usize> {
    /// The head of the queue.
    ///
    /// This value is a "stamp" consisting of an index into the buffer and a lap, but packed into a
    /// single `usize`. The lower bits represent the index, while the upper bits represent the lap.
    ///
    /// Elements are popped from the head of the queue.
    head: CacheLine<atomic!(AtomicUsize, ty)>,

    /// The tail of the queue.
    ///
    /// This value is a "stamp" consisting of an index into the buffer and a lap, but packed into a
    /// single `usize`. The lower bits represent the index, while the upper bits represent the lap.
    ///
    /// Elements are pushed into the tail of the queue.
    tail: CacheLine<atomic!(AtomicUsize, ty)>,

    /// The buffer holding slots.
    buffer: [Slot<T>; C],

    /// A stamp with the value of `{ lap: 1, index: 0 }`.
    one_lap: usize,
}

unsafe impl<T: Send, const C:usize> Sync for ArrayQueue<T, C> {}
unsafe impl<T: Send, const C: usize> Send for ArrayQueue<T, C> {}

impl<T, const C: usize> UnwindSafe for ArrayQueue<T, C> {}
impl<T, const C: usize> RefUnwindSafe for ArrayQueue<T, C> {}

impl<T, const C: usize> Default for ArrayQueue<T, C> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const C: usize> ArrayQueue<T, C> {
    /// Creates a new bounded queue with the given capacity.
    ///
    /// # Panics
    ///
    /// Panics if the capacity is zero.
    ///
    /// # Examples
    ///
    /// ```
    /// use ref_count::array_queue::ArrayQueue;
    ///
    /// let q = ArrayQueue::<i32, 3>::new();
    /// ```
    #[must_use]
    pub fn new() -> Self {
        assert_ne!(C, 0, "capacity must be non-zero");

        // Head is initialized to `{ lap: 0, index: 0 }`.
        // Tail is initialized to `{ lap: 0, index: 0 }`.
        let head = 0;
        let tail = 0;

        // Allocate a buffer of `cap` slots initialized
        // with stamps.
        let buffer= core::array::from_fn(|i| {
            // Set the stamp to `{ lap: 0, index: i }`.
            Slot {
                stamp: atomic!(usize, i),
                value: UnsafeCell::new(MaybeUninit::uninit()),
            }
        });

        // One lap is the smallest power of two greater than `cap`.
        let one_lap = (C + 1).next_power_of_two();

        Self {
            buffer,
            one_lap,
            head: CacheLine::new(atomic!(usize, head)),
            tail: CacheLine::new(atomic!(usize, tail)),
        }
    }

    fn push_or_else<F>(&self, mut value: T, f: F) -> Result<(), T>
        where
            F: Fn(T, usize, usize, &Slot<T>) -> Result<T, T>,
    {
        let backoff = Backoff::new();
        let mut tail = self.tail.load(ordering!(Relaxed));

        loop {
            // Deconstruct the tail.
            let index = tail & (self.one_lap - 1);
            let lap = tail & !(self.one_lap - 1);

            let new_tail = if index + 1 < C {
                // Same lap, incremented index.
                // Set to `{ lap: lap, index: index + 1 }`.
                tail + 1
            } else {
                // One lap forward, index wraps around to zero.
                // Set to `{ lap: lap.wrapping_add(1), index: 0 }`.
                lap.wrapping_add(self.one_lap)
            };

            // Inspect the corresponding slot.
            debug_assert!(index < C);
            let slot = unsafe { self.buffer.get_unchecked(index) };
            let stamp = slot.stamp.load(ordering!(Acquire));

            // If the tail and the stamp match, we may attempt to push.
            if tail == stamp {
                // Try moving the tail.
                match self.tail.compare_exchange_weak(
                    tail,
                    new_tail,
                    ordering!(SeqCst),
                    ordering!(Relaxed),
                ) {
                    Ok(_) => {
                        // Write the value into the slot and update the stamp.
                        unsafe {
                            slot.value.write(MaybeUninit::new(value));
                        }
                        slot.stamp.store(tail + 1, ordering!(Release));
                        return Ok(());
                    }
                    Err(t) => {
                        tail = t;
                        backoff.spin();
                    }
                }
            } else if stamp.wrapping_add(self.one_lap) == tail + 1 {
                atomic_fence!(SeqCst);
                value = f(value, tail, new_tail, slot)?;
                backoff.spin();
                tail = self.tail.load(ordering!(Relaxed));
            } else {
                // Snooze because we need to wait for the stamp to get updated.
                backoff.snooze();
                tail = self.tail.load(ordering!(Relaxed));
            }
        }
    }

    /// Attempts to push an element into the queue.
    ///
    /// # Errors
    ///
    /// If the queue is full, the element is returned back as an error.
    ///
    /// # Examples
    ///
    /// ```
    /// use ref_count::array_queue::ArrayQueue;
    ///
    /// let q = ArrayQueue::<i32, 1>::new();
    ///
    /// assert_eq!(q.push(10), Ok(()));
    /// assert_eq!(q.push(20), Err(20));
    /// ```
    pub fn push(&self, value: T) -> Result<(), T> {
        self.push_or_else(value, |v, tail, _, _| {
            let head = self.head.load(ordering!(Acquire));

            // If the head lags one lap behind the tail as well...
            if head.wrapping_add(self.one_lap) == tail {
                // ...then the queue is full.
                Err(v)
            } else {
                Ok(v)
            }
        })
    }

    /// Pushes an element into the queue, replacing the oldest element if necessary.
    ///
    /// If the queue is full, the oldest element is replaced and returned,
    /// otherwise `None` is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use ref_count::array_queue::ArrayQueue;
    ///
    /// let q = ArrayQueue::<i32, 2>::new();
    ///
    /// assert_eq!(q.force_push(10), None);
    /// assert_eq!(q.force_push(20), None);
    /// assert_eq!(q.force_push(30), Some(10));
    /// assert_eq!(q.pop(), Some(20));
    /// ```
    pub fn force_push(&self, value: T) -> Option<T> {
        self.push_or_else(value, |v, tail, new_tail, slot| {
            let head = tail.wrapping_sub(self.one_lap);
            let new_head = new_tail.wrapping_sub(self.one_lap);

            // Try moving the head.
            if self
                .head
                .compare_exchange_weak(head, new_head, ordering!(SeqCst), ordering!(Relaxed))
                .is_ok()
            {
                // Move the tail.
                self.tail.store(new_tail, ordering!(SeqCst));

                // Swap the previous value.
                let old = unsafe { slot.value.get().replace(MaybeUninit::new(v)).assume_init() };

                // Update the stamp.
                slot.stamp.store(tail + 1, ordering!(Release));

                Err(old)
            } else {
                Ok(v)
            }
        })
            .err()
    }

    /// Attempts to pop an element from the queue.
    ///
    /// If the queue is empty, `None` is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use ref_count::array_queue::ArrayQueue;
    ///
    /// let q = ArrayQueue::<i32, 32>::new();
    /// assert_eq!(q.push(10), Ok(()));
    ///
    /// assert_eq!(q.pop(), Some(10));
    /// assert!(q.pop().is_none());
    /// ```
    pub fn pop(&self) -> Option<T> {
        let backoff = Backoff::new();
        let mut head = self.head.load(ordering!(Relaxed));

        loop {
            // Deconstruct the head.
            let index = head & (self.one_lap - 1);
            let lap = head & !(self.one_lap - 1);

            // Inspect the corresponding slot.
            debug_assert!(index < C);
            let slot = unsafe { self.buffer.get_unchecked(index) };
            let stamp = slot.stamp.load(ordering!(Acquire));

            // If the stamp is ahead of the head by 1, we may attempt to pop.
            if head + 1 == stamp {
                let new = if index + 1 < C {
                    // Same lap, incremented index.
                    // Set to `{ lap: lap, index: index + 1 }`.
                    head + 1
                } else {
                    // One lap forward, index wraps around to zero.
                    // Set to `{ lap: lap.wrapping_add(1), index: 0 }`.
                    lap.wrapping_add(self.one_lap)
                };

                // Try moving the head.
                match self.head.compare_exchange_weak(
                    head,
                    new,
                    ordering!(SeqCst),
                    ordering!(Relaxed),
                ) {
                    Ok(_) => {
                        // Read the value from the slot and update the stamp.
                        let msg = unsafe { slot.value.read().assume_init() };
                        slot.stamp
                            .store(head.wrapping_add(self.one_lap), ordering!(Release));
                        return Some(msg);
                    }
                    Err(h) => {
                        head = h;
                        backoff.spin();
                    }
                }
            } else if stamp == head {
                atomic_fence!(SeqCst);
                let tail = self.tail.load(ordering!(Relaxed));

                // If the tail equals the head, that means the channel is empty.
                if tail == head {
                    return None;
                }

                backoff.spin();
                head = self.head.load(ordering!(Relaxed));
            } else {
                // Snooze because we need to wait for the stamp to get updated.
                backoff.snooze();
                head = self.head.load(ordering!(Relaxed));
            }
        }
    }

    /// Returns the capacity of the queue.
    ///
    /// # Examples
    ///
    /// ```
    /// use ref_count::array_queue::ArrayQueue;
    ///
    /// let q = ArrayQueue::<i32, 100>::new();
    ///
    /// assert_eq!(q.capacity(), 100);
    /// ```
    pub const fn capacity(&self) -> usize {
        C
    }

    /// Returns `true` if the queue is empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use ref_count::array_queue::ArrayQueue;
    ///
    /// let q = ArrayQueue::<i32, 4>::new();
    ///
    /// assert!(q.is_empty());
    /// q.push(1).unwrap();
    /// assert!(!q.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        let head = self.head.load(ordering!(SeqCst));
        let tail = self.tail.load(ordering!(SeqCst));

        // Is the tail lagging one lap behind head?
        // Is the tail equal to the head?
        //
        // Note: If the head changes just before we load the tail, that means there was a moment
        // when the channel was not empty, so it is safe to just return `false`.
        tail == head
    }

    /// Returns `true` if the queue is full.
    ///
    /// # Examples
    ///
    /// ```
    /// use ref_count::array_queue::ArrayQueue;
    ///
    /// let q = ArrayQueue::<i32, 1>::new();
    ///
    /// assert!(!q.is_full());
    /// q.push(1).unwrap();
    /// assert!(q.is_full());
    /// ```
    pub fn is_full(&self) -> bool {
        let tail = self.tail.load(ordering!(SeqCst));
        let head = self.head.load(ordering!(SeqCst));

        // Is the head lagging one lap behind tail?
        //
        // Note: If the tail changes just before we load the head, that means there was a moment
        // when the queue was not full, so it is safe to just return `false`.
        head.wrapping_add(self.one_lap) == tail
    }

    /// Returns the number of elements in the queue.
    ///
    /// # Examples
    ///
    /// ```
    /// use ref_count::array_queue::ArrayQueue;
    ///
    /// let q = ArrayQueue::<usize, 3>::new();
    /// assert_eq!(q.len(), 0);
    ///
    /// q.push(10).unwrap();
    /// assert_eq!(q.len(), 1);
    ///
    /// q.push(20).unwrap();
    /// assert_eq!(q.len(), 2);
    /// ```
    pub fn len(&self) -> usize {
        loop {
            // Load the tail, then load the head.
            let tail = self.tail.load(ordering!(SeqCst));
            let head = self.head.load(ordering!(SeqCst));

            // If the tail didn't change, we've got consistent values to work with.
            if self.tail.load(ordering!(SeqCst)) == tail {
                let head_idx = head & (self.one_lap - 1);
                let tail_idx = tail & (self.one_lap - 1);

                return queue_size!(C, head, head_idx, tail, tail_idx);
            }
        }
    }
}

impl<T, const C: usize> Drop for ArrayQueue<T, C> {
    fn drop(&mut self) {
        if mem::needs_drop::<T>() {
            // Get the index of the head.
            let head = *self.head.get_mut();
            let tail = *self.tail.get_mut();

            let head_idx = head & (self.one_lap - 1);
            let tail_idx = tail & (self.one_lap - 1);

            let len = queue_size!(C, head, head_idx, tail, tail_idx);

            // Loop over all slots that hold a message and drop them.
            for i in 0..len {
                // Compute the index of the next slot holding a message.
                let index = if head_idx + i < C {
                    head_idx + i
                } else {
                    head_idx + i - C
                };

                unsafe {
                    debug_assert!(index < C);
                    let slot = self.buffer.get_unchecked_mut(index);
                    (*slot.value.get()).assume_init_drop();
                }
            }
        }
    }
}

impl<T, const C: usize> fmt::Debug for ArrayQueue<T, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("ArrayQueue { .. }")
    }
}

impl<T, const C: usize> IntoIterator for ArrayQueue<T, C> {
    type Item = T;

    type IntoIter = IntoIter<T, C>;

    fn into_iter(self) -> Self::IntoIter {
        IntoIter { value: self }
    }
}

pub struct IntoIter<T, const C: usize> {
    value: ArrayQueue<T, C>,
}

impl<T, const C: usize> Iterator for IntoIter<T, C> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        let value = &mut self.value;
        let head = *value.head.get_mut();
        if value.head.get_mut() == value.tail.get_mut() {
            None
        } else {
            let index = head & (value.one_lap - 1);
            let lap = head & !(value.one_lap - 1);
            // SAFETY: We have mutable access to this, so we can read without
            // worrying about concurrency. Furthermore, we know this is
            // initialized because it is the value pointed at by `value.head`
            // and this is a non-empty queue.
            let val = unsafe {
                debug_assert!(index < value.buffer.len());
                let slot = value.buffer.get_unchecked_mut(index);
                slot.value.read().assume_init()
            };
            let new = if index + 1 < C {
                // Same lap, incremented index.
                // Set to `{ lap: lap, index: index + 1 }`.
                head + 1
            } else {
                // One lap forward, index wraps around to zero.
                // Set to `{ lap: lap.wrapping_add(1), index: 0 }`.
                lap.wrapping_add(value.one_lap)
            };
            *value.head.get_mut() = new;
            Some(val)
        }
    }
}