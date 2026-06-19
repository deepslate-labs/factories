//! Feature-gating macros: `cfg_<feature>! { … }` expands its body only when
//! `<feature>` is enabled, and to nothing otherwise.

/// Expand the body only if the `dynamic-dispatch` feature is enabled.
#[cfg(feature = "dynamic-dispatch")]
#[macro_export]
macro_rules! cfg_dynamic_dispatch {
    ($($body:tt)*) => { $($body)* };
}

/// The `dynamic-dispatch` feature is disabled, so this expands to nothing.
#[cfg(not(feature = "dynamic-dispatch"))]
#[macro_export]
macro_rules! cfg_dynamic_dispatch {
    ($($body:tt)*) => {};
}

/// Expand the body only if the `tokio-answer` feature is enabled.
#[cfg(feature = "tokio-answer")]
#[macro_export]
macro_rules! cfg_tokio_answer {
    ($($body:tt)*) => { $($body)* };
}

/// The `tokio-answer` feature is disabled, so this expands to nothing.
#[cfg(not(feature = "tokio-answer"))]
#[macro_export]
macro_rules! cfg_tokio_answer {
    ($($body:tt)*) => {};
}

/// Expand the body only if the `capture` feature is enabled.
#[cfg(feature = "capture")]
#[macro_export]
macro_rules! cfg_capture {
    ($($body:tt)*) => { $($body)* };
}

/// The `capture` feature is disabled, so this expands to nothing.
#[cfg(not(feature = "capture"))]
#[macro_export]
macro_rules! cfg_capture {
    ($($body:tt)*) => {};
}
