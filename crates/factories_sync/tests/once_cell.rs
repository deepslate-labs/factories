use factories_sync::{OnceCell, RawScopedMutex};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};

fn set_then_get<R: RawScopedMutex>() {
    let cell = OnceCell::<_, R>::new();

    assert_eq!(cell.get(), None);
    assert_eq!(cell.set(42u64), Ok(()));
    assert_eq!(cell.get(), Some(&42));
}

fn second_set_hands_value_back<R: RawScopedMutex>() {
    let cell = OnceCell::<_, R>::new();

    assert_eq!(cell.set(1u64), Ok(()));
    assert_eq!(cell.set(2u64), Err(2));
    assert_eq!(cell.get(), Some(&1));
}

fn get_or_init_runs_initializer_once<R: RawScopedMutex>() {
    let cell = OnceCell::<_, R>::new();
    let runs = AtomicUsize::new(0);

    let init = || {
        runs.fetch_add(1, Ordering::Relaxed);
        7u64
    };

    assert_eq!(*cell.get_or_init(init), 7);
    assert_eq!(*cell.get_or_init(init), 7);
    assert_eq!(runs.load(Ordering::Relaxed), 1);
}

fn panicking_initializer_leaves_cell_empty<R: RawScopedMutex>() {
    let cell = OnceCell::<u64, R>::new();

    let result = catch_unwind(AssertUnwindSafe(|| {
        cell.get_or_init(|| panic!("boom"));
    }));
    assert!(result.is_err());

    assert_eq!(cell.get(), None);
    assert_eq!(*cell.get_or_init(|| 9), 9);
}

struct DropCounter<'a>(&'a AtomicUsize);

impl Drop for DropCounter<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

fn initialized_value_is_dropped_with_cell<R: RawScopedMutex>() {
    let drops = AtomicUsize::new(0);

    let cell = OnceCell::<_, R>::new();
    assert!(cell.set(DropCounter(&drops)).is_ok());
    drop(cell);
    assert_eq!(drops.load(Ordering::Relaxed), 1);

    // An empty cell must not drop anything.
    drop(OnceCell::<DropCounter, R>::new());
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

macro_rules! backend_tests {
    ($module:ident, $backend:ty) => {
        mod $module {
            use super::*;

            #[test]
            fn set_then_get() {
                super::set_then_get::<$backend>();
            }

            #[test]
            fn second_set_hands_value_back() {
                super::second_set_hands_value_back::<$backend>();
            }

            #[test]
            fn get_or_init_runs_initializer_once() {
                super::get_or_init_runs_initializer_once::<$backend>();
            }

            #[test]
            fn panicking_initializer_leaves_cell_empty() {
                super::panicking_initializer_leaves_cell_empty::<$backend>();
            }

            #[test]
            fn initialized_value_is_dropped_with_cell() {
                super::initialized_value_is_dropped_with_cell::<$backend>();
            }

            #[test]
            fn cell_of_sync_data_is_sync() {
                fn require_sync<T: Sync>() {}
                require_sync::<OnceCell<u64, $backend>>();
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

static STATIC_CELL: OnceCell<u64> = OnceCell::new();

#[test]
fn static_cell_is_usable() {
    assert_eq!(*STATIC_CELL.get_or_init(|| 11), 11);
}
