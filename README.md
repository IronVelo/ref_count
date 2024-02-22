# ref_count

`ref_count` is a high-performance, robust, `no_std`, `no_alloc`, async reference counting crate designed for Rust 
environments that require efficiency and low overhead. It leverages core futures and core wakers, alongside a lock-free 
queue, to manage synchronization without blocking or spinning, making it ideal for use in embedded systems, real-time 
applications, or any scenario where dynamic memory allocation and standard library features are not available or desired.

## Features

- **High Performance**: Optimized for speed and efficiency in environments where resources are limited.
- **Robustness**: Carefully implemented to ensure thread safety and free of deadlocks, especially when dealing with 
                  exclusive references, verified with `loom` among other measures.
- **`no_std` and `no_alloc`**: Works in `no_std` environments without requiring dynamic memory allocation.
- **Asynchronous**: Utilizes futures and wakers from the core library to handle waiting without blocking or spinning.
- **Exclusive References**: Supports exclusive references, allowing for the creation of higher level primitives such as 
                            readers-writer locks. 
- **Fair**: Taking advantage of a lock-free queue, priority inversion and starvation are prevented.
- **Environment Agnostic**: From seL4 to Linux, this crate was designed to not care. This also applies to async 
                            runtimes, working with whatever your preference is.

## Installation

Add `ref_count` to your `Cargo.toml`:

```toml
[dependencies]
ref_count = "0.1.2"
```

## Example Usage 

Creating an `RwLock`

```rust
use ref_count::{RefCount, Ref, ExclusiveRef, maybe_await};
use core::cell::UnsafeCell;

const MAX_WAITERS: usize = 32;

struct RwLock<T> {
    data: UnsafeCell<T>,
    state: RefCount<MAX_WAITERS>
}

struct ReadGuard<'lock, T> {
    _ref: Ref<'lock, MAX_WAITERS>,
    data: &'lock T
}

struct WriteGuard<'lock, T> {
    _ref: ExclusiveRef<'lock, MAX_WAITERS>,
    data: &'lock mut T
}

impl<T> RwLock<T> {
    fn new(t: T) -> Self {
        Self {
            data: UnsafeCell::new(t),
            state: RefCount::new()
        }
    }
    
    async fn read(&self) -> ReadGuard<T> {
        let r = maybe_await!(self.state.get_ref());
        ReadGuard {
            _ref: r,
            // SAFETY: Multiple read guards can be created as `RefCount` ensures that
            // no exclusive references are granted when shared references exist.
            data: unsafe { &*self.data.get() }
        }
    }
    
    async fn write(&self) -> WriteGuard<T> {
        let e = maybe_await!(self.state.get_exclusive_ref());
        WriteGuard {
            _ref: e,
            // SAFETY: Only one `WriteGuard` can be created at a time as `RefCount` ensures
            // exclusive access to the data.
            data: unsafe { &mut *self.data.get() }
        }
    }
}
```

### Simplicity in Action

The example above was crafted in under two minutes, highlighting the straightforward and user-friendly design of 
`ref_count`. This simplicity is at the core of the crate's philosophy, ensuring developers can implement robust, 
`no_std`, `no_alloc`, async reference counting without a steep learning curve. Whether in embedded systems, real-time 
applications, or other efficiency-critical environments, `ref_count` aims to simplify the development process while 
maintaining high performance and safety standards.

## Performance Focus: Standard vs. Exclusive References

In the development of `ref_count`, it was observed that in many applications, particularly those using read-write locks,
the frequency of read operations significantly surpasses that of write operations. This pattern is not merely anecdotal 
but a well-documented phenomenon across a variety of fields, from file systems to database management. With this 
insight, `ref_count` was deliberately engineered to excel in these common scenarios, thereby ensuring that the 
operations performed most frequently are also the most efficient.

By focusing on enhancing the performance of acquiring and releasing standard references, `ref_count` achieves over a 50% 
improvement in these operations compared to its leading competitors. For exclusive references, the design prioritizes 
acquisition speed and efficiency for waiting threads, rather than concentrating solely on the speed of release 
operations.

### Benefits

- **Optimized Read Operations**: Provides faster access for the more commonly needed read operations, enhancing overall 
                                 application performance.
- **Well-Managed Write Access**: Ensures efficient and equitable management of write operations, contributing to the 
                                 stability and reliability of the system.
- **Versatility**: Designed to accommodate a broad spectrum of applications, from those predominantly requiring read 
                   access to those where timely write access is crucial.