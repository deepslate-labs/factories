use crate::raw::{DefaultRawMutex, RawScopedMutex};
use core::cell::UnsafeCell;
use core::fmt::{Debug, Formatter};
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU8, Ordering};

/// A cell that can be written once and read lock-free afterwards.
pub struct OnceCell<T, R: RawScopedMutex = DefaultRawMutex> {
    state: AtomicU8,
    raw: R,
    value: UnsafeCell<MaybeUninit<T>>,
}

const EMPTY: u8 = 0;
const FULL: u8 = 1;

impl<T, R: RawScopedMutex> OnceCell<T, R> {
    /// Create an empty cell, usable in `const` contexts.
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(EMPTY),
            raw: R::INIT,
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// The stored value, or `None` if nothing was stored yet.
    pub fn get(&self) -> Option<&T> {
        if self.state.load(Ordering::Acquire) == FULL {
            // Safety: `FULL` is published with release ordering strictly
            // after the value was written, and the value is never unset.
            Some(unsafe { (*self.value.get()).assume_init_ref() })
        } else {
            None
        }
    }

    /// Store `value` if the cell is still empty, else hand it back.
    pub fn set(&self, value: T) -> Result<(), T> {
        self.raw.with_lock(|| {
            if self.state.load(Ordering::Acquire) == FULL {
                return Err(value);
            }

            // Safety: still `EMPTY` and we hold the write lock.
            unsafe { (*self.value.get()).write(value) };
            self.state.store(FULL, Ordering::Release);
            Ok(())
        })
    }

    /// The stored value, initializing it with `f` if the cell is empty.
    ///
    /// Attempting to access the cell recursively is implementation defined
    /// behavior, but never unsafe (will usually just deadlock or panic, but
    /// never corrupt memory or hand out mutable aliased references).
    pub fn get_or_init(&self, f: impl FnOnce() -> T) -> &T {
        if let Some(value) = self.get() {
            return value;
        }

        self.raw.with_lock(|| {
            if self.state.load(Ordering::Acquire) != FULL {
                let value = f();
                // Safety: still `EMPTY` and we hold the write lock. The
                // write happens only when `f` returned, so a panicking
                // initializer leaves the cell empty.
                unsafe { (*self.value.get()).write(value) };
                self.state.store(FULL, Ordering::Release);
            }
        });

        self.get()
            .expect("cell is initialized right above and never unset")
    }
}

impl<T, R: RawScopedMutex> Drop for OnceCell<T, R> {
    fn drop(&mut self) {
        if *self.state.get_mut() == FULL {
            // Safety: `FULL` guarantees the value was written.
            unsafe { self.value.get_mut().assume_init_drop() };
        }
    }
}

impl<T, R: RawScopedMutex> Default for OnceCell<T, R> {
    fn default() -> Self {
        Self::new()
    }
}

// Safety: same story as std::sync::OnceLock - the cell moves `T` in from one
// thread and hands out `&T` to many, so sending needs `T: Send` and sharing
// needs both (any sharer may be the writer, and `&T` goes to every thread).
unsafe impl<T: Send, R: RawScopedMutex + Send> Send for OnceCell<T, R> {}
unsafe impl<T: Send + Sync, R: RawScopedMutex + Sync> Sync for OnceCell<T, R> {}

impl<T: Debug, R: RawScopedMutex> Debug for OnceCell<T, R> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("OnceCell").field(&self.get()).finish()
    }
}
