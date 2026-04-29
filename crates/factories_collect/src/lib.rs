#![no_std]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

/// Global entry in a collection.
pub struct GlobalCollectionEntry<T: 'static> {
    value: &'static T,
    next: UnsafeCell<Option<&'static Self>>,
    initialized: AtomicBool,
}

// SAFETY: The UnsafeCell<Option<&'static Self>> in `next` is only written during `register()`
// before the entry is published to the list (guarded by the `initialized` CAS + the list CAS).
// After publication, `next` is read-only.
unsafe impl<T: Sync> Sync for GlobalCollectionEntry<T> {}

impl<T> GlobalCollectionEntry<T> {
    /// Create a new global collection entry that points to some T.
    pub const fn new(value: &'static T) -> Self {
        Self {
            value,
            next: UnsafeCell::new(None),
            initialized: AtomicBool::new(false),
        }
    }

    /// Retrieve the value in this entry.
    pub const fn value(&self) -> &'static T {
        self.value
    }

    /// Retrieve the next entry in the collection, if any.
    pub const fn next(&self) -> Option<&'static Self> {
        // SAFETY: We only ever write &'static entries into next
        unsafe { self.next.get().read() }
    }
}

/// A global collection of items.
pub struct GlobalCollection<T: 'static> {
    head: AtomicPtr<GlobalCollectionEntry<T>>,
}

impl<T> GlobalCollection<T> {
    /// Create a new global collection.
    pub const fn new() -> Self {
        Self {
            head: AtomicPtr::new(core::ptr::null_mut()),
        }
    }

    /// Register a new entry into this collection.
    pub fn register(&self, entry: &'static GlobalCollectionEntry<T>) {
        if entry.initialized.swap(true, Ordering::Relaxed) {
            // Initialized already
            return;
        }

        let mut head = self.head.load(Ordering::Relaxed);
        loop {
            // SAFETY: We have acquired the lock on entry by swapping initialized to true
            unsafe { entry.next.get().write(head.as_ref()) };

            let new_head = core::ptr::from_ref(entry).cast_mut();

            match self.head.compare_exchange(head, new_head, Ordering::Release, Ordering::Relaxed) {
                Ok(_) => return,
                Err(current_head) => head = current_head,
            }
        }
    }

    /// Create an iterator over the global collection.
    pub const fn iter(&'_ self) -> GlobalCollectionIter<'_, T> {
        GlobalCollectionIter::new(self)
    }

    /// Load the head of this collection.
    pub(crate) fn head(&self) -> Option<&'static GlobalCollectionEntry<T>> {
        // SAFETY: We only ever write &'static entries into head
        unsafe { self.head.load(Ordering::Acquire).as_ref() }
    }
}

impl<'a, T> IntoIterator for &'a GlobalCollection<T> {
    type Item = &'a T;
    type IntoIter = GlobalCollectionIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        GlobalCollectionIter::new(self)
    }
}

pub struct GlobalCollectionIter<'a, T: 'static> {
    state: GlobalCollectionIterState<'a, T>,
}

enum GlobalCollectionIterState<'a, T: 'static> {
    Uninitialized(&'a GlobalCollection<T>),
    Iterating(&'a GlobalCollectionEntry<T>),
    Done,
}

impl<'a, T> GlobalCollectionIter<'a, T> {
    /// Create a new iterator over the given global collection.
    pub const fn new(collection: &'a GlobalCollection<T>) -> Self {
        Self { state: GlobalCollectionIterState::Uninitialized(collection) }
    }
}

impl<'a, T> Iterator for GlobalCollectionIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = match &mut self.state {
            GlobalCollectionIterState::Uninitialized(collection) => match collection.head() {
                Some(v) => v,
                None => {
                    self.state = GlobalCollectionIterState::Done;
                    return None;
                }
            }
            GlobalCollectionIterState::Iterating(entry) => *entry,
            GlobalCollectionIterState::Done => return None,
        };

        match entry.next() {
            None => self.state = GlobalCollectionIterState::Done,
            Some(v) => self.state = GlobalCollectionIterState::Iterating(v),
        }

        Some(entry.value())
    }
}

/// Register a function to run on binary load.
///
/// This is inherently unsafe as this may execute the function before the `main` function is
/// invoked.
#[macro_export]
macro_rules! unsafe_run_on_binary_load {
    ($f:expr) => {
        #[allow(non_upper_case_globals, unsafe_code, unused_unsafe)]
        const _: () = {
            #[cfg_attr(
                any(target_os = "linux", target_os = "android"),
                unsafe(link_section = ".text.startup")
            )]
            unsafe extern "C" fn factories_collect_initializer() {
                unsafe { ($f)() };
            }

            #[used]
            #[cfg_attr(
                any(
                    target_os = "linux",
                    target_os = "android",
                    target_os = "dragonfly",
                    target_os = "freebsd",
                    target_os = "haiku",
                    target_os = "illumos",
                    target_os = "netbsd",
                    target_os = "nto",
                    target_os = "openbsd",
                    target_os = "vxworks",
                    target_os = "none",
                    target_os = "espidf",
                    target_family = "wasm"
                ),
                unsafe(link_section = ".init_array")
            )]
            #[cfg_attr(
                any(target_os = "macos", target_os = "ios"),
                unsafe(link_section = "__DATA,__mod_init_func,mod_init_funcs")
            )]
            #[cfg_attr(windows, unsafe(link_section = ".CRT$XCU"))]
            static __CTOR: unsafe extern "C" fn() = factories_collect_initializer;
        };
    };
}

/// Create a global collection.
///
/// This is just a convenience macro that ensures the correct usage.
#[macro_export]
macro_rules! global_collection {
    ($t:ty) => {{
        static COLLECTION: $crate::GlobalCollection<$t> = $crate::GlobalCollection::new();
        &COLLECTION
    }};
}

/// Register an entry into a global collection.
#[macro_export]
macro_rules! register_global_collection_entry {
    ($collection:path, $entry:path) => {
        $crate::unsafe_run_on_binary_load!(|| {
            // Typecheck...
            let collection = $collection as &'static $crate::GlobalCollection::<_>;
            collection.register(&$entry);
        });
    };
}
