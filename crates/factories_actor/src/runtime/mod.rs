//! Concrete implementations: channels, run loops and task spawners.

#[cfg(feature = "kanal-runtime")]
pub mod kanal;

#[cfg(any(feature = "tokio-runtime", feature = "tokio-lock"))]
pub mod tokio;

#[cfg(feature = "dynamic-dispatch")]
pub mod registry;

pub mod concurrent_loop;
pub mod defaults;
pub mod init;
pub mod lock;
pub mod result;
pub mod routing;
pub mod sequential_loop;
pub mod template;

/// Register a dynamically dispatched message handler *if* the
/// `dynamic-dispatch` feature is enabled; expands to nothing otherwise.
///
/// This is the macro counterpart of the [`defaults`] aliases: code generated
/// by the `#[messages]` attribute cannot see this crate's features, so it
/// emits this registration unconditionally and lets the feature decide here.
/// Also useful for hand-written handlers that must compile with and without
/// the feature.
#[cfg(feature = "dynamic-dispatch")]
#[macro_export]
macro_rules! register_dynamic_handler_if_enabled {
    ($actor:ty, $message:ty) => {
        $crate::register_dynamic_handler!($actor, $message);
    };
}

/// Register a dynamically dispatched message handler *if* the
/// `dynamic-dispatch` feature is enabled; expands to nothing otherwise.
///
/// The feature is disabled, so this expands to nothing.
#[cfg(not(feature = "dynamic-dispatch"))]
#[macro_export]
macro_rules! register_dynamic_handler_if_enabled {
    ($actor:ty, $message:ty) => {};
}
