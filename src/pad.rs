//! Prevent False Sharing

use core::ops::{Deref, DerefMut};
use core::fmt::{self, Formatter, Debug, Display};

#[cfg_attr(
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "powerpc64",
    ),
    repr(align(128))
)]
#[cfg_attr(
    any(
        target_arch = "arm",
        target_arch = "mips",
        target_arch = "mips32r6",
        target_arch = "mips64",
        target_arch = "mips64r6",
        target_arch = "sparc",
        target_arch = "hexagon",
    ),
    repr(align(32))
)]
#[cfg_attr(target_arch = "m68k", repr(align(16)))]
#[cfg_attr(target_arch = "s390x", repr(align(256)))]
#[cfg_attr(
    not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "powerpc64",
        target_arch = "arm",
        target_arch = "mips",
        target_arch = "mips32r6",
        target_arch = "mips64",
        target_arch = "mips64r6",
        target_arch = "sparc",
        target_arch = "hexagon",
        target_arch = "m68k",
        target_arch = "s390x",
    )),
    repr(align(64))
)]
pub struct CacheLine<T>(pub T);

impl<T> CacheLine<T> {
    #[inline]
    pub const fn new(val: T) -> Self {
        Self(val)
    }
}

impl<T> Deref for CacheLine<T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for CacheLine<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> From<T> for CacheLine<T> {
    #[inline]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}


impl<T: Debug> Debug for CacheLine<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("CacheLine")
            .field("value", &self.0)
            .finish()
    }
}

impl<T: Display> Display for CacheLine<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                                                      write!(f, "{}", self.0)
                                                                             }
}