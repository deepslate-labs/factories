//! Shared test helpers.

use core::any::TypeId;

/// Assert that two types are the same type.
pub fn assert_type_eq<T: 'static, U: 'static>() {
    assert_eq!(
        TypeId::of::<T>(),
        TypeId::of::<U>(),
        "associated type mismatch"
    );
}
