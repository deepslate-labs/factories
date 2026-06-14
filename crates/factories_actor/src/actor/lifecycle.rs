//! Actor lifecycle: the [`StopReason`] handed to
//! [`Actor::on_stop`](crate::actor::Actor::on_stop).
//!
//! The hooks themselves - [`on_start`](crate::actor::Actor::on_start) and
//! [`on_stop`](crate::actor::Actor::on_stop) - are provided methods on
//! [`Actor`](crate::actor::Actor): implement them directly on a hand-written
//! actor, or write `#[on_start]` / `#[on_stop]` methods in a `#[messages]` block
//! and let the derive route them. Both return a handler-style
//! [`IntoRunLoopWork`](crate::actor::work::IntoRunLoopWork), so the loop's
//! [`WorkConverter`](crate::actor::ActorRunLoop::WorkConverter) governs their
//! `Send`-ness exactly as it does message-handler work; the default is a no-op
//! ([`NoWork`](crate::actor::work::NoWork)).

use crate::actor::Actor;
use core::fmt::{Debug, Formatter};

/// Why an actor's run loop is stopping, handed to
/// [`on_stop`](crate::actor::Actor::on_stop).
pub enum StopReason<'a, A: Actor + ?Sized> {
    /// The event source signalled completion (e.g. the mailbox closed) and the
    /// loop drained without failing.
    Finished,

    /// A handler - or the start hook - failed the actor. Carries the error that
    /// was recorded with [`ActorContext::fail`](crate::actor::ActorContext::fail).
    Failed(&'a A::Error),
}

// `&A::Error` is `Copy` regardless of `A::Error`, so the reason is freely
// copyable; the manual impls avoid the spurious `A: Copy`/`Clone` bounds a
// derive would add.
impl<A: Actor + ?Sized> Copy for StopReason<'_, A> {}

impl<A: Actor + ?Sized> Clone for StopReason<'_, A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A: Actor + ?Sized> Debug for StopReason<'_, A>
where
    A::Error: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Finished => f.write_str("Finished"),
            Self::Failed(error) => f.debug_tuple("Failed").field(error).finish(),
        }
    }
}
