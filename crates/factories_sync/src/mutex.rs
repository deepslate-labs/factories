use crate::raw::{DefaultRawMutex, RawScopedMutex};
use core::cell::UnsafeCell;
use core::fmt::{Debug, Formatter};

/// Mutual exclusion around `T` with closure-scoped access.
///
/// The backend defaults to this crate's feature selection
/// ([`DefaultRawMutex`]); see the crate docs for the model. There is no
/// poisoning: after a panic inside [`with_lock`](Self::with_lock) the mutex
/// stays usable.
pub struct Mutex<T, R: RawScopedMutex = DefaultRawMutex> {
    raw: R,
    data: UnsafeCell<T>,
}

impl<T, R: RawScopedMutex> Mutex<T, R> {
    /// Create an unlocked mutex around `value`.
    pub const fn new(value: T) -> Self {
        Self {
            raw: R::INIT,
            data: UnsafeCell::new(value),
        }
    }

    /// Run `f` with exclusive access to the data.
    ///
    /// Locking is not reentrant: calling `with_lock` on the same mutex from
    /// within `f` is implementation defined behavior, but never unsafe
    /// (will usually just deadlock or panic, but never hand out an alias
    /// mutable reference or otherwise corrupt data).
    pub fn with_lock<U>(&self, f: impl FnOnce(&mut T) -> U) -> U {
        // Safety: `RawScopedMutex` guarantees no other closure (cross-thread
        // or reentrant) runs concurrently, so the `&mut` is exclusive.
        self.raw.with_lock(|| f(unsafe { &mut *self.data.get() }))
    }
}

// Safety: the mutex hands out `&mut T` only under the raw lock's exclusion
// guarantee, so sharing the mutex is moving/borrowing `T` across threads.
unsafe impl<T: Send, R: RawScopedMutex + Send> Send for Mutex<T, R> {}
unsafe impl<T: Send, R: RawScopedMutex + Sync> Sync for Mutex<T, R> {}

impl<T, R: RawScopedMutex> Debug for Mutex<T, R> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Mutex").finish_non_exhaustive()
    }
}
