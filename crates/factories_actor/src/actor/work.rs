//! The run loop's unit of work.
//!
//! A run loop drives *work*, not necessarily futures. A handler returns whatever
//! its loop's [`WorkConverter`](crate::actor::ActorRunLoop::WorkConverter)
//! accepts; [`IntoRunLoopWork`] turns that into the converter's own
//! [`Erased`](WorkConverter::Erased) representation. A future is just *one* thing
//! a converter accepts.
//!
//! The currency that crosses the (uniform) dispatcher fn-pointer is
//! [`ErasedWork`]: a fully opaque cell that mandates *no* drive protocol. It is a
//! pure transient - built and unpacked back to the converter's typed `Erased`
//! within a single synchronous
//! [`dispatch_onto_loop`](crate::actor::dispatch::DispatchedActorMessage::dispatch_onto_loop)
//! call, never held across an `.await` and never sent - so it needs no `Send` of
//! its own (it is plainly `!Send`). The typed `Erased` it yields carries the real
//! bounds, which is why `dispatch_onto_loop` can be a public (`unsafe`) building
//! block for hand-rolled run loops.

use alloc::boxed::Box;
use core::future::Future;
use core::pin::Pin;

trait Payload<'a> {}
impl<'a, T: 'a> Payload<'a> for T {}

/// The uniform, fully type-erased unit of work crossing the dispatcher
/// fn-pointer. Carries no protocol: the dispatch site casts it back to its
/// converter's [`Erased`](WorkConverter::Erased) and the loop drives *that*
/// however it likes.
pub struct ErasedWork<'a>(Box<dyn Payload<'a> + 'a>);

impl<'a> ErasedWork<'a> {
    /// Pack any payload. The concrete type is forgotten; only the box's drop
    /// glue is retained (in its vtable), so dropping the cell un-unpacked still
    /// runs the payload's destructor.
    ///
    /// Safe: the cell is `!Send`, so a packed `!Send` payload can never be moved
    /// to another thread through it.
    pub fn pack<T: 'a>(value: T) -> Self {
        ErasedWork(Box::new(value))
    }

    /// Recover the payload by value.
    ///
    /// # Safety
    /// `T` must be exactly the type that was packed. In the crate this is upheld
    /// by [`dispatch_onto_loop`](crate::actor::dispatch::DispatchedActorMessage::dispatch_onto_loop):
    /// the dispatcher was bound for actor `A`, so it packed `A`'s converter's
    /// [`Erased`](WorkConverter::Erased) - the same fact that already makes that
    /// method `unsafe`.
    pub unsafe fn unpack<T: 'a>(self) -> T {
        // fat `*mut dyn Payload` -> thin `*mut T` keeps the data address; the
        // allocation *is* a `T`, so reclaiming it as `Box<T>` is sound.
        let raw = Box::into_raw(self.0).cast::<T>();
        *unsafe { Box::from_raw(raw) }
    }
}

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
