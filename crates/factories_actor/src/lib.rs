#![no_std]

extern crate alloc;

// Tests run on std targets (panic unwinding, catch_unwind).
#[cfg(test)]
extern crate std;

pub mod actor;
pub mod message;
pub mod runtime;
pub mod spawn;

/// Internal `tracing` instrumentation helpers (no-ops without the feature).
pub(crate) mod obs;

/// Method-style message handlers: marks an inherent impl block, every
/// `#[handler]` method additionally becomes a message handler. See the macro
/// documentation for details.
#[cfg(feature = "derive")]
pub use factories_actor_macro::messages;

/// Declare an actor protocol: a trait whose methods name messages, plus a
/// concrete erased handle guaranteeing those messages bind. See the macro
/// documentation for details.
#[cfg(feature = "derive")]
pub use factories_actor_macro::protocol;

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
