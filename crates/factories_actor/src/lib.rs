#![no_std]

extern crate alloc;

// Tests run on std targets (panic unwinding, catch_unwind); the `capture`
// feature also needs std for its loop-scoped thread-local sender context.
#[cfg(any(test, feature = "capture"))]
extern crate std;

/// The pointer-keyed extension system lives in its own crate now; re-exported
/// at the root so `factories_actor::declare_extension!` (and the facade's, and
/// the in-crate `crate::declare_extension!`) keep working unchanged.
pub use factories_extension::declare_extension;

pub mod actor;
pub mod message;
pub mod runtime;
pub mod spawn;

/// `cfg_<feature>! { … }` gating macros, usable inside the crate's exported macros
/// (they expand downstream). Exported at the crate root via `#[macro_export]`.
mod cfg;

/// Internal `tracing` instrumentation helpers (no-ops without the feature).
pub(crate) mod obs;

/// Append-only capture/audit log of actor spawns, deaths, and message edges.
/// Behind the `capture` feature.
#[cfg(feature = "capture")]
pub mod capture;
mod util;

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
