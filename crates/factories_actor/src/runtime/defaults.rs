//! Default component selections for derived actors.
//!
//! `#[derive(Actor)]` fills every associated type that is not explicitly
//! configured in `#[actor(...)]` with an alias from this module. The
//! indirection is what makes feature-dependent defaults possible in the first
//! place: the derive macro is compiled separately and cannot see which
//! features of *this* crate are enabled, so it unconditionally emits paths
//! into this module and lets `cfg` resolve them here.
//!
//! Two consequences worth knowing:
//!
//! - The defaults are ordinary, nameable type aliases. Writing
//!   `type Channel = defaults::DefaultChannel;` by hand is exactly equivalent
//!   to letting the derive fill it in - the macro adds no capability.
//! - A default whose backing feature is disabled simply does not exist, and
//!   the derive output fails with "cannot find type `Default...`" pointing at
//!   this module. Enable the feature listed on the alias or configure the
//!   component explicitly.

/// Default channel:
/// [`TokioMpscActorChannel`](crate::runtime::tokio::TokioMpscMultiLineActorChannel), a
/// cancellation-safe `tokio::sync::mpsc` mailbox.
///
/// Requires the `tokio-runtime` feature.
#[cfg(feature = "tokio-runtime")]
pub type DefaultChannel = crate::runtime::tokio::TokioMpscActorChannel;

/// Default actor error type: actors that don't configure one cannot fail.
pub type DefaultError = core::convert::Infallible;

/// Default runtime binder with the `dynamic-dispatch` feature enabled: the
/// global handler registry. Messages registered via
/// [`register_dynamic_handler!`](crate::register_dynamic_handler) bind
/// dynamically.
#[cfg(feature = "dynamic-dispatch")]
pub type DefaultRuntimeBinder<A> = crate::runtime::registry::RegistryBinder<A>;

/// Default runtime binder without the `dynamic-dispatch` feature: dynamic
/// sends never bind, only static dispatch is available.
#[cfg(not(feature = "dynamic-dispatch"))]
pub type DefaultRuntimeBinder<A> = <A as static_only::Select>::Binder;

// We need the `DefaultRuntimeBinder` to have the same generic arity regardless
// of the enabled features. To facilitate this, we have to slightly
// hack around here.
//
// Use the trait to completely ignore the generic parameter and then select a
// non generic type.
#[cfg(not(feature = "dynamic-dispatch"))]
mod static_only {
    pub trait Select {
        type Binder;
    }

    impl<A: ?Sized> Select for A {
        type Binder = crate::actor::StaticOnlyBinder;
    }
}

/// Default lock strategy:
/// [`UnguardedLock`](crate::runtime::lock::UnguardedLock) - no synchronization,
/// relying on the serialized [`DefaultRunLoop`] for exclusivity.
///
/// This pairs with the default [`SequentialRunLoop`](crate::runtime::sequential_loop::SequentialRunLoop):
/// dispatches never overlap, so the state needs no real lock and both `&self`
/// (shared) and `&mut self` (exclusive) handlers work with zero overhead.
/// Dependency-free (`core` atomics only).
///
/// Overriding [`DefaultRunLoop`] to a concurrent loop while leaving this default
/// is a compile error - `UnguardedLock`'s access modes require
/// [`SerializedDispatch`](crate::actor::SerializedDispatch) - so opting into
/// concurrency forces choosing a real lock (e.g.
/// [`TokioRwLock`](crate::runtime::tokio::TokioRwLock)).
pub type DefaultLockStrategy<A> = crate::runtime::lock::UnguardedLock<A>;

/// Default run loop:
/// [`SequentialRunLoop`](crate::runtime::sequential_loop::SequentialRunLoop) -
/// one message handled to completion before the next is pulled (dependency-free,
/// always available).
///
/// This is the least-surprising actor model: messages are processed in order,
/// one at a time. A handler that `.await`s blocks the actor until it resolves
/// (head-of-line blocking); for actors that need overlapping dispatch, opt into
/// [`ConcurrentRunLoop`](crate::runtime::concurrent_loop::ConcurrentRunLoop) with
/// a real lock.
pub type DefaultRunLoop<A> = crate::runtime::sequential_loop::SequentialRunLoop<A>;
