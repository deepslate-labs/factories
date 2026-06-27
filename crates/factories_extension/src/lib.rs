//! Pointer-keyed, type-erased context values.
//!
//! An [`Extension<T>`] is an RTTI-style descriptor declared as a `&'static`
//! (via [`declare_extension!`]); its *address* is the key. A value is stored
//! under that key in an [`ExtensionSet`] and read back by handing the same
//! descriptor to [`ExtensionSet::get`].
//!
//! This was originally part of `factories_actor`; it lives in its own crate so
//! pieces that want the mechanism without the actor framework (e.g. the capture
//! config) can depend on just this.
#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::marker::PhantomData;
use core::ptr::NonNull;

use factories_rtti::{BasicTypeRtti, CloneRtti};

/// Descriptor for an injected, type-erased context value of type `T`.
///
/// Declared as a `&'static` via [`declare_extension!`]; the descriptor's
/// address is its key in an [`ExtensionSet`]. The descriptor carries `T`'s
/// erasure glue ([`BasicTypeRtti`] for drop), so the set can manage stored
/// values without naming `T`.
pub struct Extension<T> {
    name: &'static str,
    basic: BasicTypeRtti,
    /// `Some` marks the extension inheritable and carries the glue to clone its
    /// value onto a child; `None` marks it local.
    clone: Option<CloneRtti>,
    _t: PhantomData<fn() -> T>,
}

impl<T: 'static> Extension<T> {
    /// A non-inheritable extension: it is read by the actor it is set on, but is
    /// not copied to actors that actor spawns. Use through [`declare_extension!`].
    pub const fn local(name: &'static str) -> Self {
        Self {
            name,
            basic: BasicTypeRtti::new::<T>(),
            clone: None,
            _t: PhantomData,
        }
    }
}

impl<T: Clone + 'static> Extension<T> {
    /// An inheritable extension: besides being read by the actor it is set on,
    /// it is cloned onto actors spawned through that actor's context (see
    /// [`ExtensionSet::inherit_inheritable_from`]). The `Clone` bound lands here,
    /// at the declaration, exactly where inheritance needs it. Use through
    /// [`declare_extension!`].
    pub const fn inheritable(name: &'static str) -> Self {
        Self {
            name,
            basic: BasicTypeRtti::new::<T>(),
            clone: Some(CloneRtti::new::<T>()),
            _t: PhantomData,
        }
    }
}

impl<T> Extension<T> {
    /// The human-readable label (defaults to the declaring binding's name).
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The descriptor's identity: its address, used as the [`ExtensionSet`] key.
    ///
    /// Distinct `static` descriptors have distinct addresses, so the identity is
    /// unique per declaration.
    pub fn identity(&self) -> usize {
        core::ptr::from_ref(self).addr()
    }

    /// Whether this extension is copied onto spawned children.
    pub fn is_inheritable(&self) -> bool {
        self.clone.is_some()
    }
}

/// One stored, type-erased extension value plus the glue to manage it.
struct Entry {
    /// The descriptor's [`Extension::identity`].
    key: usize,
    /// `Box::into_raw` of the stored `T`, type-erased.
    value: NonNull<c_void>,
    /// Erasure glue copied from the descriptor at insert (drop).
    basic: BasicTypeRtti,
    /// The descriptor's inheritance glue: `Some` clone-RTTI if inheritable.
    clone: Option<CloneRtti>,
}

/// An append-built, immutable-after-spawn map of [`Extension`] values keyed by
/// descriptor address.
///
/// Backed by a linear-scanned `Vec`: holders carry a handful of extensions at
/// most, so this beats a hashed map on both cache behavior and `no_std`
/// simplicity.
pub struct ExtensionSet {
    entries: Vec<Entry>,
}

impl ExtensionSet {
    /// An empty set.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Store `value` under `ext`'s key.
    pub fn insert<T: Send + Sync + 'static>(&mut self, ext: &'static Extension<T>, value: T) {
        let raw = Box::into_raw(Box::new(value)).cast::<c_void>();
        let value = NonNull::new(raw).expect("boxed pointer is never null");
        self.set_raw(ext.identity(), value, ext.basic, ext.clone);
    }

    /// Insert or replace the entry under `key`, dropping any value it displaces.
    ///
    /// Replacement makes a later explicit [`insert`](Self::insert) win over an
    /// earlier value under the same key (inheritance, by contrast, never
    /// overwrites - see [`inherit_inheritable_from`](Self::inherit_inheritable_from)).
    fn set_raw(
        &mut self,
        key: usize,
        value: NonNull<c_void>,
        basic: BasicTypeRtti,
        clone: Option<CloneRtti>,
    ) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.key == key) {
            let old_value = entry.value;
            let old_basic = entry.basic;
            entry.value = value;
            entry.basic = basic;
            entry.clone = clone;
            // SAFETY: `old_value` was stored as `Box::into_raw` of a `T` whose
            // RTTI is `old_basic`, so this frees exactly that one `Box<T>`.
            unsafe { old_basic.drop_boxed_ptr(old_value.as_ptr()) };
        } else {
            self.entries.push(Entry {
                key,
                value,
                basic,
                clone,
            });
        }
    }

    /// Borrow the value stored under `ext`, if any.
    pub fn get<T: 'static>(&self, ext: &'static Extension<T>) -> Option<&T> {
        let key = ext.identity();
        let entry = self.entries.iter().find(|entry| entry.key == key)?;

        // SAFETY: A value is only ever inserted under this key via
        // `insert(&'static Extension<T>, T)`, whose signature pins the stored
        // type to the descriptor's `T`. Distinct `static` descriptors have
        // distinct addresses (see `Extension::identity`), so a key match proves
        // the stored value is a `Box<T>` of this very `T`.
        Some(unsafe { &*entry.value.as_ptr().cast::<T>() })
    }

    /// Copy every *inheritable* entry of `src` whose key this set does not
    /// already hold, cloning each value through its [`CloneRtti`]; local entries
    /// are skipped.
    ///
    /// An already-present value - explicit or earlier-inherited - is never
    /// overwritten, so an explicit [`insert`](Self::insert) wins over inheritance
    /// *regardless of call order*. This is how a child actor receives its
    /// spawner's inheritable context.
    pub fn inherit_inheritable_from(&mut self, src: &ExtensionSet) {
        for entry in &src.entries {
            let Some(clone) = entry.clone else {
                continue; // local: never inherited
            };
            if self.entries.iter().any(|existing| existing.key == entry.key) {
                continue; // already set here: inheritance never overwrites
            }

            // SAFETY: `entry.value` is `Box::into_raw` of the entry's `T`, and
            // `clone` is that same `T`'s `CloneRtti`, so this clones exactly one
            // correctly-typed `T` into a fresh box.
            let cloned = unsafe { clone.clone_into_box(entry.value.as_ptr().cast_const()) };
            let value = NonNull::new(cloned.cast::<c_void>()).expect("clone returns a non-null box");
            self.entries.push(Entry {
                key: entry.key,
                value,
                basic: entry.basic,
                clone: entry.clone,
            });
        }
    }
}

impl Default for ExtensionSet {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ExtensionSet {
    fn drop(&mut self) {
        for entry in &mut self.entries {
            // SAFETY: `value` is `Box::into_raw` of a `T`, and `basic` is that
            // same `T`'s RTTI (copied from the descriptor at insert), so this
            // reconstructs and drops exactly one correctly-typed `Box<T>`.
            unsafe { entry.basic.drop_boxed_ptr(entry.value.as_ptr()) };
        }
    }
}

// SAFETY: a value is only ever stored via `insert<T: Send + Sync + 'static>`
// (or cloned from another set's such value during inheritance), so every erased
// payload is itself `Send + Sync`. The set is otherwise a `Vec` of those
// payloads plus `Copy` RTTI, so moving it across threads and sharing `&` of it
// are both sound. The raw `NonNull` is the only reason these are not derived.
unsafe impl Send for ExtensionSet {}
unsafe impl Sync for ExtensionSet {}

/// Declare a `&'static` [`Extension`] descriptor.
///
/// ```ignore
/// declare_extension!(pub CACHE: MyCache, local);          // label defaults to "CACHE"
/// declare_extension!(SCRATCH: Scratch, local, "scratch"); // explicit label
/// ```
#[macro_export]
macro_rules! declare_extension {
    ($(#[$meta:meta])* $vis:vis $name:ident : $t:ty, local) => {
        $crate::declare_extension!($(#[$meta])* $vis $name : $t, local, ::core::stringify!($name));
    };
    ($(#[$meta:meta])* $vis:vis $name:ident : $t:ty, local, $label:expr) => {
        $(#[$meta])*
        $vis const $name: &'static $crate::Extension<$t> = const {
            static VALUE: $crate::Extension<$t> = $crate::Extension::local($label);
            &VALUE
        };
    };
    ($(#[$meta:meta])* $vis:vis $name:ident : $t:ty, inheritable) => {
        $crate::declare_extension!($(#[$meta])* $vis $name : $t, inheritable, ::core::stringify!($name));
    };
    ($(#[$meta:meta])* $vis:vis $name:ident : $t:ty, inheritable, $label:expr) => {
        $(#[$meta])*
        $vis const $name: &'static $crate::Extension<$t> = const {
            static VALUE: $crate::Extension<$t> = $crate::Extension::inheritable($label);
            &VALUE
        };
    };
}

#[cfg(test)]
mod tests {
    use crate::ExtensionSet;

    use core::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn get_returns_inserted_value() {
        crate::declare_extension!(FOO: u32, local);

        let mut set = ExtensionSet::new();
        set.insert(FOO, 42u32);

        assert_eq!(set.get(FOO), Some(&42));
    }

    #[test]
    fn dropping_the_set_drops_its_values() {
        static DROPS: AtomicUsize = AtomicUsize::new(0);

        struct Tracked;
        impl Drop for Tracked {
            fn drop(&mut self) {
                DROPS.fetch_add(1, Ordering::SeqCst);
            }
        }

        crate::declare_extension!(T: Tracked, local);

        {
            let mut set = ExtensionSet::new();
            set.insert(T, Tracked);
            assert_eq!(DROPS.load(Ordering::SeqCst), 0, "value lives while the set holds it");
        }

        assert_eq!(DROPS.load(Ordering::SeqCst), 1, "value dropped exactly once with the set");
    }

    #[test]
    fn inherit_copies_inheritable_but_not_local() {
        crate::declare_extension!(KEPT: u32, inheritable);
        crate::declare_extension!(DROPPED: u32, local);

        let mut parent = ExtensionSet::new();
        parent.insert(KEPT, 1u32);
        parent.insert(DROPPED, 2u32);

        let mut child = ExtensionSet::new();
        child.inherit_inheritable_from(&parent);

        assert_eq!(child.get(KEPT), Some(&1), "inheritable flows to the child");
        assert_eq!(child.get(DROPPED), None, "local stays with the parent");
    }

    #[test]
    fn inheritance_never_overwrites_an_existing_value() {
        crate::declare_extension!(K: u32, inheritable);

        let mut parent = ExtensionSet::new();
        parent.insert(K, 100u32);

        let mut child = ExtensionSet::new();
        child.insert(K, 1u32); // an explicit value, set before inheriting
        child.inherit_inheritable_from(&parent);

        // Explicit wins regardless of order: inheritance fills absent keys only.
        assert_eq!(child.get(K), Some(&1), "an explicit value is never clobbered by inheritance");
    }

    #[test]
    fn insert_replaces_value_under_the_same_key() {
        static DROPS: AtomicUsize = AtomicUsize::new(0);

        struct Tracked(u32);
        impl Drop for Tracked {
            fn drop(&mut self) {
                DROPS.fetch_add(1, Ordering::SeqCst);
            }
        }

        crate::declare_extension!(K: Tracked, local);

        let mut set = ExtensionSet::new();
        set.insert(K, Tracked(1));
        set.insert(K, Tracked(2));

        assert_eq!(set.get(K).map(|t| t.0), Some(2), "the later insert wins");
        assert_eq!(DROPS.load(Ordering::SeqCst), 1, "the replaced value is dropped at once");
    }

    #[test]
    fn distinct_descriptors_of_the_same_type_are_distinct_keys() {
        crate::declare_extension!(A: u32, local);
        crate::declare_extension!(B: u32, local);

        let mut set = ExtensionSet::new();
        set.insert(A, 1u32);
        set.insert(B, 2u32);

        assert_eq!(set.get(A), Some(&1));
        assert_eq!(set.get(B), Some(&2));
    }

    #[test]
    fn extension_set_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ExtensionSet>();
    }
}
