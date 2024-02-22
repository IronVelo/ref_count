//! Future impl for `Ref` and `ExclusiveRef`

use core::pin::{Pin, pin};
use core::future::Future;
use core::task::{Context, Poll};
use crate::{ExclusiveRef, Ref, RefCount};
use crate::Waiter;

/// # `MaybeFuture`
///
/// Either ready or a future to avoid unnecessary polling. Generally used in combination with
/// the `maybe_await` macro for improved readability.
///
/// # Example
///
/// ```
/// # use ref_count::{RefCount, futures::MaybeFuture};
/// # let ref_count = RefCount::<32>::new();
/// match ref_count.get_ref() {
///     MaybeFuture::Future(_f) => {
///         // await this
///     },
///     MaybeFuture::Ready(_ref) => {}
/// };
/// ```
/// Or just
/// ```
/// # use ref_count::{RefCount, maybe_await};
/// async fn example() {
///     let ref_count = RefCount::<32>::new();
///     let reference = maybe_await!(ref_count.get_ref());
/// }
/// ```
pub enum MaybeFuture<R, F: Future<Output = R>> {
    Ready(R),
    Future(F)
}

macro_rules! try_register_future {
    ($registered:expr, $queue:expr, $context:ident) => {
        if !$registered.load(ordering!(Acquire)) {
            match $queue.push(unsafe { Waiter::new($context.waker().clone(), &mut $registered) }) {
                Ok(()) => $registered.store(true, ordering!(Relaxed)),
                Err(waker) => waker.wake()
            };
        }
    };
}

pub struct FutureRef<'count, const MAX_WAITING: usize> {
    pub src: &'count RefCount<MAX_WAITING>,
    registered: atomic!(AtomicBool, ty)
}

impl<'count, const MAX_WAITING: usize> FutureRef<'count, MAX_WAITING> {
    #[must_use]
    #[cfg(not(loom))]
    pub const fn new(counter: &'count RefCount<MAX_WAITING>) -> Self {
        Self {
            src: counter,
            registered: atomic!(bool, false)
        }
    }

    #[must_use]
    #[cfg(loom)]
    pub fn new(counter: &'count RefCount<MAX_WAITING>) -> Self {
        Self {
            src: counter,
            registered: atomic!(bool, false)
        }
    }
}

impl<'count, const MAX_WAITING: usize> Future for FutureRef<'count, MAX_WAITING> {
    type Output = Ref<'count, MAX_WAITING>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.src.is_blocked() {
            try_register_future!(self.registered, self.src.waiter_queue, cx);
        } else {
            match self.src.try_get_ref() {
                Some(reference) => return Poll::Ready(reference),
                None => {
                    // simply due to high contention, request to be polled again.
                    cx.waker().wake_by_ref();
                }
            }
        }

        Poll::Pending
    }
}

pub struct FutureBlockedRefs<'count, const MAX_WAITING: usize> {
    pub src: &'count RefCount<MAX_WAITING>,
    registered: atomic!(AtomicBool, ty)
}

impl<'count, const MAX_WAITING: usize> FutureBlockedRefs<'count, MAX_WAITING> {
    #[inline]
    #[cfg(not(loom))]
    pub const fn new(count: &'count RefCount<MAX_WAITING>) -> Self {
        Self {
            src: count,
            registered: atomic!(bool, false)
        }
    }

    #[inline]
    #[cfg(loom)]
    pub fn new(count: &'count RefCount<MAX_WAITING>) -> Self {
        Self {
            src: count,
            registered: atomic!(bool, false)
        }
    }
}

impl<'count, const MAX_WAITING: usize> Future for FutureBlockedRefs<'count, MAX_WAITING> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if unsafe { self.src.try_block_refs() } {
            Poll::Ready(())
        } else {
            if self.src.is_blocked() {
                try_register_future!(self.registered, self.src.waiter_queue, cx);
            } else {
                slight_spin!(0, fake);
                // simply failing due to high contention, request to be polled
                cx.waker().wake_by_ref();
            }

            Poll::Pending
        }
    }
}

pub struct FutureExclusiveRef<'count, const MAX_WAITING: usize> {
    pub blocked_fut: Option<FutureBlockedRefs<'count, MAX_WAITING>>,
    pub src: &'count RefCount<MAX_WAITING>,
}

impl<'count, const MAX_WAITING: usize> FutureExclusiveRef<'count, MAX_WAITING> {
    pub const fn new(
        src: &'count RefCount<MAX_WAITING>,
        blocked_fut: Option<FutureBlockedRefs<'count, MAX_WAITING>>
    ) -> Self {
        Self {
            blocked_fut,
            src,
        }
    }
}

impl<'count, const MAX_WAITING: usize> Future for FutureExclusiveRef<'count, MAX_WAITING> {
    type Output = ExclusiveRef<'count, MAX_WAITING>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(blocked_fut) = &mut self.blocked_fut {
            match pin!(blocked_fut).poll(cx) {
                Poll::Ready(()) => self.blocked_fut = None,
                Poll::Pending => return Poll::Pending
            };
        }

        if self.src.no_standard_refs() {
            Poll::Ready(unsafe { ExclusiveRef::new(self.src) })
        } else {
            // we are the one currently blocking, as we are just waiting for refs to reach 0
            // request to be polled again
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}