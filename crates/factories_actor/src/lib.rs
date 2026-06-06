#![no_std]

extern crate alloc;

pub mod message;
pub mod actor;
pub mod runtime;
pub mod spawn;

// Re-exported for use by the declaration macros; not public API.
#[doc(hidden)]
pub use factories_rtti;

#[doc(hidden)]
pub use paste;

#[doc(hidden)]
pub mod __private {
    pub use alloc::boxed::Box;
}
