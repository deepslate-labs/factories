use factories_sync::{Mutex, RawScopedMutex};
use std::panic::{AssertUnwindSafe, catch_unwind};

fn lock_mutates_and_returns<R: RawScopedMutex>() {
    let mutex = Mutex::<_, R>::new(3u64);

    let doubled = mutex.with_lock(|value| {
        *value += 1;
        *value * 2
    });

    assert_eq!(doubled, 8);
    assert_eq!(mutex.with_lock(|value| *value), 4);
}

/// No poisoning: a panic inside the closure leaves the lock usable and the
/// data reachable.
fn lock_usable_after_panic_in_closure<R: RawScopedMutex>() {
    let mutex = Mutex::<_, R>::new(7u64);

    let result = catch_unwind(AssertUnwindSafe(|| {
        mutex.with_lock(|_| panic!("boom"));
    }));
    assert!(result.is_err());

    assert_eq!(mutex.with_lock(|value| *value), 7);
}

macro_rules! backend_tests {
    ($module:ident, $backend:ty) => {
        mod $module {
            use super::*;

            #[test]
            fn lock_mutates_and_returns() {
                super::lock_mutates_and_returns::<$backend>();
            }

            #[test]
            fn lock_usable_after_panic_in_closure() {
                super::lock_usable_after_panic_in_closure::<$backend>();
            }

            #[test]
            fn mutex_of_send_only_data_is_sync() {
                fn require_sync<T: Sync>() {}
                require_sync::<Mutex<core::cell::Cell<u8>, $backend>>();
            }
        }
    };
}

#[cfg(feature = "std")]
backend_tests!(std_backend, factories_sync::StdRawMutex);
#[cfg(feature = "critical-section")]
backend_tests!(
    critical_section_backend,
    factories_sync::CriticalSectionRawMutex
);

static STATIC_MUTEX: Mutex<u64> = Mutex::new(0);

#[test]
fn static_mutex_is_usable() {
    STATIC_MUTEX.with_lock(|value| *value += 1);
    assert!(STATIC_MUTEX.with_lock(|value| *value) >= 1);
}
