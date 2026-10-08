/// A raw scoped lock: mutual exclusion without data.
///
/// # Safety
///
/// An implementation must guarantee that while one [`with_lock`] closure
/// runs, no other `with_lock` closure of the same instance can run - across
/// threads *and* reentrantly on the same thread (a reentrant attempt must
/// deadlock or panic, never nest).
///
/// [`with_lock`]: Self::with_lock
pub unsafe trait RawScopedMutex {
    /// An unlocked instance.
    const INIT: Self;

    /// Run `f` while holding the lock.
    fn with_lock<R>(&self, f: impl FnOnce() -> R) -> R;
}

/// The backend picked by this crate's feature selection: [`StdRawMutex`]
/// when `std` is enabled, [`CriticalSectionRawMutex`] otherwise.
#[cfg(feature = "std")]
pub type DefaultRawMutex = StdRawMutex;

/// The backend picked by this crate's feature selection: `StdRawMutex` when
/// `std` is enabled, [`CriticalSectionRawMutex`] otherwise.
#[cfg(all(not(feature = "std"), feature = "critical-section"))]
pub type DefaultRawMutex = CriticalSectionRawMutex;

/// A raw mutex backed by a dedicated `std::sync::Mutex`, without poisoning:
/// a panic inside the closure leaves the lock usable.
///
/// Reentrant locking is forbidden by the [`RawScopedMutex`] contract, this
/// backend inherits `std::sync::Mutex` behavior (deadlock or panic).
#[cfg(feature = "std")]
pub struct StdRawMutex {
    inner: std::sync::Mutex<()>,
}

#[cfg(feature = "std")]
unsafe impl RawScopedMutex for StdRawMutex {
    const INIT: Self = Self {
        inner: std::sync::Mutex::new(()),
    };

    fn with_lock<R>(&self, f: impl FnOnce() -> R) -> R {
        // No poisoning by design (see crate docs): a panic in an earlier
        // closure must not brick the lock.
        let _guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f()
    }
}

/// A raw mutex that enters the global critical section.
#[cfg(feature = "critical-section")]
pub struct CriticalSectionRawMutex {
    taken: core::sync::atomic::AtomicBool,
}

#[cfg(feature = "critical-section")]
unsafe impl RawScopedMutex for CriticalSectionRawMutex {
    const INIT: Self = Self {
        taken: core::sync::atomic::AtomicBool::new(false),
    };

    fn with_lock<R>(&self, f: impl FnOnce() -> R) -> R {
        use core::sync::atomic::{AtomicBool, Ordering};

        critical_section::with(|_| {
            // The critical section already excludes other threads; the flag
            // only catches same-thread reentrancy (`with` nests). Orderings
            // are belt-and-braces for exotic critical-section providers.
            if self.taken.swap(true, Ordering::Acquire) {
                panic!("reentrant lock of the same critical-section mutex");
            }

            // Reset on unwind too: factories catches handler panics, so a
            // panic inside `f` must not wedge the flag forever.
            struct Reset<'a>(&'a AtomicBool);
            impl Drop for Reset<'_> {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _reset = Reset(&self.taken);

            f()
        })
    }
}
