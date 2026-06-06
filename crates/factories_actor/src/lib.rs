#![no_std]

extern crate alloc;

// Tests run on std targets (panic unwinding, catch_unwind).
#[cfg(test)]
extern crate std;

pub mod message;
pub mod actor;
pub mod runtime;
pub mod spawn;

// Re-exported for use by the declaration macros; not public API.
#[doc(hidden)]
pub use factories_rtti;

#[cfg(feature = "dynamic-dispatch")]
#[doc(hidden)]
pub use factories_collect;

#[doc(hidden)]
pub use paste;

#[doc(hidden)]
pub mod __private {
    pub use alloc::boxed::Box;
}
