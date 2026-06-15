//! The run loop's unit of work.
//!
//! A run loop drives *work*, not necessarily futures. A handler returns whatever
//! its loop's [`WorkConverter`](crate::actor::ActorRunLoop::WorkConverter)
//! accepts; [`IntoRunLoopWork`] turns that into the converter's own
//! [`Erased`](WorkConverter::Erased) representation. A future is just *one* thing
//! a converter accepts.
//!
//! The dispatcher fn-pointer is uniform across every actor/message pair, so it
//! cannot name the converter's [`Erased`](WorkConverter::Erased) in its return
//! type. Rather than heap-erase the value to launder it across that boundary, the
//! dispatch site
//! ([`dispatch_onto_loop`](crate::actor::dispatch::DispatchedActorMessage::dispatch_onto_loop))
//! provides a correctly-typed, uninitialized stack slot and the dispatcher writes
//! its `Erased` into it through a thin `*mut ()`. No heap, no boxing: the value
//! is moved once into storage the caller already owns, and the caller - which
//! knows the concrete `Erased` from its actor type parameter - reads it straight
//! back. This is why `dispatch_onto_loop` can be a public (`unsafe`) building
//! block for hand-rolled run loops.

use alloc::boxed::Box;
use core::future::Future;
use core::pin::Pin;

/// Selects how a handler's return becomes a run loop's work, and what that work
/// *is*. A run loop names one of these via
/// [`WorkConverter`](crate::actor::ActorRunLoop::WorkConverter).
///
/// This trait mandates nothing about how work is driven - that is the loop's
/// business once it has [`Erased`](Self::Erased) back. A loop that drives by
/// polling additionally implements [`FutureWorkConverter`].
pub trait WorkConverter {
    /// This loop's unit of work, valid for `'a`.
    type Erased<'a>;

    /// Work that does nothing.
    fn empty<'a>() -> Self::Erased<'a>;
}

/// A [`WorkConverter`] whose work is driven by polling a `Send` future - the
/// contract the standard (work-stealing) run loops require.
///
/// Kept separate from [`WorkConverter`] so the base trait stays protocol-free: a
/// loop with a non-poll protocol implements only `WorkConverter` and supplies its
/// own dispatch path.
pub trait FutureWorkConverter: WorkConverter {
    /// View this converter's work as the `Send` future the standard loops drive.
    fn into_future<'a>(work: Self::Erased<'a>) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

/// Turn a handler's return value into a run loop's
/// [`Erased`](WorkConverter::Erased) work.
///
/// `C` is the loop's converter; impls keyed on different converters never
/// overlap (the trait solver sees distinct trait references), so a loop can
/// accept futures, plain values, or anything else without those impls
/// conflicting.
pub trait IntoRunLoopWork<C: WorkConverter> {
    /// Convert into the converter's erased work, valid for `'a`.
    fn into_erased<'a>(self) -> C::Erased<'a>
    where
        Self: Sized + 'a;
}

/// Erase `work` through converter `C`.
///
/// A turbofish-friendly free function around
/// [`IntoRunLoopWork::into_erased`] - `into_work::<Conv, _>(work)` names the
/// converter explicitly while the work type and lifetime are inferred. Dispatcher
/// declaration sites use it, where `C` is the actor's
/// [`WorkConverter`](crate::actor::ActorRunLoop::WorkConverter) and any `Send`
/// requirement it imposes is checked against the concrete work right there.
pub fn into_work<'a, C, W>(work: W) -> C::Erased<'a>
where
    C: WorkConverter,
    W: IntoRunLoopWork<C> + 'a,
{
    work.into_erased()
}

#[derive(Debug, Default, Copy, Clone)]
pub struct SendFutureConverter;

impl WorkConverter for SendFutureConverter {
    type Erased<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

    fn empty<'a>() -> Self::Erased<'a> {
        Box::pin(async {})
    }
}

impl FutureWorkConverter for SendFutureConverter {
    fn into_future<'a>(work: Self::Erased<'a>) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        work
    }
}

impl<F: Future<Output = ()> + Send> IntoRunLoopWork<SendFutureConverter> for F {
    fn into_erased<'a>(self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>
    where
        F: 'a,
    {
        Box::pin(self)
    }
}

/// Work that does nothing, for *any* converter.
///
/// The no-op an unimplemented lifecycle hook
/// ([`Actor::on_start`](crate::actor::Actor::on_start) /
/// [`on_stop`](crate::actor::Actor::on_stop)) returns: it erases to
/// [`WorkConverter::empty`] for whatever converter the loop uses, so this single
/// value satisfies [`IntoRunLoopWork`] for every loop without per-converter impls.
#[derive(Debug, Default, Copy, Clone)]
pub struct NoWork;

impl<C: WorkConverter> IntoRunLoopWork<C> for NoWork {
    fn into_erased<'a>(self) -> C::Erased<'a>
    where
        Self: 'a,
    {
        C::empty()
    }
}
