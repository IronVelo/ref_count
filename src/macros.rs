#[cfg(loom)]
macro_rules! ordering {
    (ty) => {
        ::loom::sync::atomic::Ordering
    };
    ($order:ident) => {
        ::loom::sync::atomic::Ordering::$order
    };
}

#[cfg(not(loom))]
macro_rules! ordering {
    (ty) => {
        ::core::sync::atomic::Ordering
    };
    ($order:ident) => {
        ::core::sync::atomic::Ordering::$order
    };
}

#[cfg(loom)]
macro_rules! atomic {
    (u8, $value:expr) => {
        ::loom::sync::atomic::AtomicU8::new($value)
    };
    (u16, $value:expr) => {
        ::loom::sync::atomic::AtomicU16::new($value)
    };
    (u32, $value:expr) => {
        ::loom::sync::atomic::AtomicU32::new($value)
    };
    (u64, $value:expr) => {
        ::loom::sync::atomic::AtomicU64::new($value)
    };
    (usize, $value:expr) => {
        ::loom::sync::atomic::AtomicUsize::new($value)
    };
    ($kind:ident) => {
        atomic!($kind, 0)
    };
    ($kind:ident, ty) => {
        ::loom::sync::atomic::$kind
    };
}

#[cfg(not(loom))]
macro_rules! atomic {
    (u8, $value:expr) => {
        ::core::sync::atomic::AtomicU8::new($value)
    };
    (u16, $value:expr) => {
        ::core::sync::atomic::AtomicU16::new($value)
    };
    (u32, $value:expr) => {
        ::core::sync::atomic::AtomicU32::new($value)
    };
    (u64, $value:expr) => {
        ::core::sync::atomic::AtomicU64::new($value)
    };
    (usize, $value:expr) => {
        ::core::sync::atomic::AtomicUsize::new($value)
    };
    ($kind:ident) => {
        atomic!($kind, 0)
    };
    ($kind:ident, ty) => {
        ::core::sync::atomic::$kind
    };
}

#[cfg(not(loom))]
macro_rules! atomic_fence {
    ($ordering:ident) => {
        ::core::sync::atomic::fence(ordering!($ordering))
    };
}

#[cfg(loom)]
macro_rules! atomic_fence {
    ($ordering:ident) => {
        ::loom::sync::atomic::fence(ordering!($ordering))
    };
}

#[cfg(not(loom))]
macro_rules! slight_spin {
    ($attempts:expr) => {
        for _ in 0..(2 << ($attempts & ((1 << 3) - 1))) {
            core::hint::spin_loop();
        }
    };
    ($attempts:expr, fake) => {};
}

#[cfg(loom)]
macro_rules! slight_spin {
    ($attempts:expr) => {
        for _ in 0..(2 << ($attempts & ((1 << 3) - 1))) {
            loom::hint::spin_loop();
        }
    };
    ($attempts:expr, fake) => {
        loom::hint::spin_loop();
    };
}

macro_rules! state_const {
    ($c:ident) => {
        $crate::consts::$c
    };
}

macro_rules! weak_try_update_ref_count {
    ($state:expr, $cur_state:ident, $next_state:ident, $ok:expr, |$new_state:ident| $err:expr) => {
        match $state.compare_exchange_weak(
            $cur_state, $next_state,
            ordering!(SeqCst), ordering!(SeqCst)
        ) {
            Ok(_) => $ok,
            Err($new_state) => $err
        }
    };
    (incr, $state:expr, $cur_state:ident, $next_state:ident, $ok:expr) => {
        weak_try_update_ref_count!($state, $cur_state, $next_state, $ok, |__s| {
            $cur_state = __s & state_const!(REF_COUNT_MASK);
            $next_state = $cur_state + state_const!(REF_INCR);
        })
    };
    (block, $state:expr, $cur_state:ident, $next_state:ident, $ok:expr) => {
        weak_try_update_ref_count!($state, $cur_state, $next_state, $ok, |__s| {
            $cur_state = __s & state_const!(REF_COUNT_MASK);
            $next_state = $cur_state | state_const!(REF_BLOCKED);
        })
    };
}

macro_rules! many_weak_try_incr_refs {
    ($state:expr, $cur_state:ident, $next_state:ident, $ok:expr) => {{
        weak_try_update_ref_count!(incr, $state, $cur_state, $next_state, $ok);
        weak_try_update_ref_count!(incr, $state, $cur_state, $next_state, $ok);
        weak_try_update_ref_count!(incr, $state, $cur_state, $next_state, $ok);
        weak_try_update_ref_count!($state, $cur_state, $next_state, $ok, |_s| {})
    }};
}

macro_rules! standard_ref_count {
    ($state:expr) => {
        $state >> state_const!(REF_COUNT_SHIFT)
    };
}

// IMPORTANT NOTE REGARDING POTENTIAL UB IF CHANGED:
//
// If the potential output of this macro is ever anything other than 0 or 1 the State enum's
// from_raw associated function MUST BE UPDATED to correspond with the changes.
macro_rules! block_state {
    ($state:expr) => {
        $state & state_const!(STATE_MASK)
    };
}