use crate::message::envelope::MessageEnvelope;
use alloc::boxed::Box;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

/// The handler future returned once lock acquisition has completed.
///
/// Run loops push this into their concurrent work set after awaiting the
/// outer [`BoxedAcquireFuture`].
///
/// Intentionally NOT `Send`: thread-safety of handler futures is the run
/// loop's concern, expressed through its [`crate::actor::DispatchDemand`] and
/// enforced at dispatcher declaration sites. Run loops that demand
/// [`crate::actor::ThreadSafe`] futures may reclaim the erased proof via
/// [`AssertSend`].
pub type BoxedHandlerFuture<'ctx> = Pin<Box<dyn Future<Output = ()> + 'ctx>>;

/// The acquire-then-build future returned directly by a dispatcher.
///
/// Awaiting this drives lock acquisition. The result is a [`BoxedHandlerFuture`]
/// that holds the lock guard and runs the user's `MessageHandler::handle`. Run
/// loops await this inline with `mailbox.next()` so acquisition order matches
/// mailbox order; the resolved handler future then runs concurrently with
/// other in-flight handlers.
pub type BoxedAcquireFuture<'ctx> = Pin<Box<dyn Future<Output = BoxedHandlerFuture<'ctx>> + 'ctx>>;

pub type ActorMessageDispatcherHandler = for<'ctx> unsafe fn(
    DispatchContextPtr<'ctx>,
    DispatchedActorMessageContext,
) -> BoxedAcquireFuture<'ctx>;

/// Type-erased pointer to an actor's dispatch context, tagged with the lifetime
/// during which the pointee is valid.
///
/// The lifetime tag ties the future returned by [`ActorMessageDispatcher::invoke`]
/// to the dispatch context, so the borrow checker can enforce that the future is
/// dropped before the dispatch context goes away.
#[derive(Copy, Clone)]
#[repr(transparent)]
pub struct DispatchContextPtr<'ctx> {
    raw: *const core::ffi::c_void,
    _data: PhantomData<&'ctx ()>,
}

impl<'ctx> DispatchContextPtr<'ctx> {
    /// Construct a dispatch context pointer from a typed reference.
    pub const fn new<C>(ctx: &'ctx C) -> Self {
        Self {
            raw: core::ptr::from_ref(ctx).cast(),
            _data: PhantomData,
        }
    }

    /// Recover the typed reference from the pointer.
    ///
    /// # Safety
    /// The pointer must have been constructed via [`Self::new`] from a `&'ctx C`
    /// with the exact same `C`.
    pub const unsafe fn cast_as<C>(self) -> &'ctx C {
        // SAFETY: Per the contract, the raw pointer came from `&'ctx C`.
        unsafe { &*self.raw.cast::<C>() }
    }
}

#[derive(Copy, Clone)]
pub struct ActorMessageDispatcher {
    handler: ActorMessageDispatcherHandler,
}

impl ActorMessageDispatcher {
    /// Create a new message dispatcher using the given handler.
    pub const fn new(handler: ActorMessageDispatcherHandler) -> Self {
        Self { handler }
    }

    /// Invoke the dispatcher, producing an outer future that drives lock
    /// acquisition and resolves to the inner handler future.
    ///
    /// The two-tier shape lets the run loop sequence lock acquisition through
    /// the mailbox reader: await the outer inline with `mailbox.next()`, then
    /// push the resolved handler future into the concurrent work set.
    ///
    /// # Safety
    /// - `dispatch_context` must have been constructed from a reference to the
    ///   dispatch context of the actor type this dispatcher was bound for.
    /// - The envelope in `message_context` must carry a message of the type this
    ///   dispatcher was bound for.
    /// - The caller must invoke this on the actor's thread.
    pub unsafe fn invoke(
        self,
        dispatch_context: DispatchContextPtr,
        message_context: DispatchedActorMessageContext,
    ) -> BoxedAcquireFuture {
        unsafe { (self.handler)(dispatch_context, message_context) }
    }
}

impl core::fmt::Debug for ActorMessageDispatcher {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ActorMessageDispatcher")
            .field("handler", &(self.handler as *const ()))
            .finish()
    }
}

// SAFETY: The dispatcher is just a function pointer; thread-safety is enforced
//         at the call site via the safety contract on `invoke`.
unsafe impl Send for ActorMessageDispatcher {}

// SAFETY: Same as above — a fn pointer is trivially Sync.
unsafe impl Sync for ActorMessageDispatcher {}

/// An [`ActorMessageDispatcher`] stamped with the actor/message pair it was
/// declared for, carrying the proof obligations of its declaration site.
///
/// Values of this type are produced by
/// [`declare_static_dispatcher!`](crate::declare_static_dispatcher) and live as
/// the [`crate::actor::MessageHandler::DISPATCHER`] associated const.
pub struct StaticDispatcher<A: ?Sized, M: ?Sized> {
    dispatcher: ActorMessageDispatcher,
    _types: PhantomData<(fn(&A), fn(&M))>,
}

impl<A: ?Sized, M: ?Sized> StaticDispatcher<A, M> {
    /// Create a static dispatcher from a raw dispatcher.
    ///
    /// # Safety
    /// The caller must ensure that the dispatcher
    /// - dispatches envelopes carrying messages of type `M` to actors of type `A`, and
    /// - satisfies the [`crate::actor::DispatchDemand`] of `A`'s run loop.
    ///
    /// Use [`declare_static_dispatcher!`](crate::declare_static_dispatcher), which
    /// upholds both by construction.
    pub const unsafe fn new_unchecked(dispatcher: ActorMessageDispatcher) -> Self {
        Self {
            dispatcher,
            _types: PhantomData,
        }
    }

    /// Unwrap the raw dispatcher.
    pub const fn into_dispatcher(self) -> ActorMessageDispatcher {
        self.dispatcher
    }
}

impl<A: ?Sized, M: ?Sized> core::fmt::Debug for StaticDispatcher<A, M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StaticDispatcher")
            .field("dispatcher", &self.dispatcher)
            .finish()
    }
}

/// Declare the [`crate::actor::MessageHandler::DISPATCHER`] constant for an
/// actor/message pair.
///
/// Expands to a dispatcher whose body is checked against the
/// [`crate::actor::DispatchDemand`] of the actor's run loop *at this declaration
/// site*, where the concrete handler future types are known and their auto traits
/// leak. A handler future that doesn't satisfy the demand (e.g. a `!Send` future
/// on a [`crate::actor::ThreadSafe`] loop) fails to compile right here.
#[macro_export]
macro_rules! declare_static_dispatcher {
    ($actor:ty, $message:ty) => {{
        unsafe fn invoke<'ctx>(
            dispatch_context: $crate::actor::dispatch::DispatchContextPtr<'ctx>,
            message_context: $crate::actor::dispatch::DispatchedActorMessageContext,
        ) -> $crate::actor::dispatch::BoxedAcquireFuture<'ctx> {
            type RunLoop = <$actor as $crate::actor::Actor>::RunLoop;
            type Demand = <RunLoop as $crate::actor::ActorRunLoop<$actor>>::Demand;
            type Mode = <$actor as $crate::actor::MessageHandler<$message>>::AccessMode;

            // SAFETY: The `StaticDispatcher` contract guarantees this dispatcher is
            //         only invoked with the dispatch context of `$actor`'s run loop.
            let dispatch_context = unsafe {
                dispatch_context
                    .cast_as::<<RunLoop as $crate::actor::ActorRunLoop<$actor>>::DispatchContext>()
            };

            let acquire = $crate::actor::demand_check::<Demand, _>(async move {
                let guard = <Mode as $crate::actor::AccessMode<$actor>>::acquire(
                    $crate::actor::ActorRunLoopDispatchContext::lock_strategy(dispatch_context),
                )
                .await;

                let (envelope, span) = message_context.into_parts();

                // SAFETY: The `StaticDispatcher` contract guarantees the envelope
                //         carries a message of type `$message`.
                let ctx = unsafe {
                    $crate::actor::MessageHandlerContext::<$message, $actor, Mode>::new_unchecked(
                        guard,
                        $crate::actor::ActorRunLoopDispatchContext::shared_state(dispatch_context),
                        envelope,
                    )
                };

                // Calling `handle` only constructs the future; it runs once polled.
                // The demand check here is THE enforcement point of the run loop's
                // demand on the concrete handler future.
                let handler = $crate::actor::demand_check::<Demand, _>(span.instrument(
                    <$actor as $crate::actor::MessageHandler<$message>>::handle(ctx),
                ));

                $crate::__private::Box::pin(handler)
                    as $crate::actor::dispatch::BoxedHandlerFuture<'ctx>
            });

            $crate::__private::Box::pin(acquire)
        }

        // SAFETY: `invoke` above is generated for exactly this actor/message pair
        //         and demand-checked against the actor's run loop.
        unsafe {
            $crate::actor::dispatch::StaticDispatcher::<$actor, $message>::new_unchecked(
                $crate::actor::dispatch::ActorMessageDispatcher::new(invoke),
            )
        }
    }};
}

pub use declare_static_dispatcher;

#[derive(Debug)]
pub struct DispatchedActorMessage {
    dispatcher: ActorMessageDispatcher,
    context: DispatchedActorMessageContext,
}

impl DispatchedActorMessage {
    /// Create a dispatched message from a dispatcher and its context.
    ///
    /// # Safety
    /// The dispatcher must be able to dispatch the envelope in `context` to the
    /// actor this message will be delivered to, and must satisfy the
    /// [`crate::actor::DispatchDemand`] of that actor's run loop. Dispatchers
    /// obtained from [`crate::actor::MessageHandler::DISPATCHER`] or a
    /// [`crate::actor::ActorRuntimeBinder`] uphold this by their own contracts.
    pub const unsafe fn new(
        dispatcher: ActorMessageDispatcher,
        context: DispatchedActorMessageContext,
    ) -> Self {
        Self {
            dispatcher,
            context,
        }
    }

    /// Access the envelope carried by this message.
    pub fn envelope(&self) -> &MessageEnvelope {
        &self.context.envelope
    }

    /// Decompose the message into its parts.
    pub fn into_parts(self) -> (ActorMessageDispatcher, DispatchedActorMessageContext) {
        (self.dispatcher, self.context)
    }

    /// Dispatch the message onto the actor loop.
    ///
    /// # Safety
    /// The type A must be the target type to which the actor message was bound.
    ///
    /// In addition to that must this method be called in an appropriate dispatch context. For most
    /// actors this means in the actor dispatch loop, unless pass through messaging is enabled
    /// for the actor.
    pub unsafe fn dispatch_onto_loop<A: crate::actor::Actor + ?Sized>(
        self,
        dispatch_context: &<A::RunLoop as crate::actor::ActorRunLoop<A>>::DispatchContext,
    ) -> BoxedAcquireFuture<'_> {
        let (dispatcher, message_context) = self.into_parts();

        unsafe { dispatcher.invoke(DispatchContextPtr::new(dispatch_context), message_context) }
    }
}

// SAFETY: We don't make any assumptions in the dispatch message itself about thread safety
//         nor do we touch anything directly without escalating the unsafe to the caller.
//         Channels that transport this across threads must verify
//         `MessageEnvelope::is_sendable` first (see `ActorChannel`).
unsafe impl Send for DispatchedActorMessage {}

#[derive(Debug)]
pub struct DispatchedActorMessageContext {
    pub envelope: MessageEnvelope,

    #[cfg(feature = "tracing")]
    pub span: tracing::Span,
}

impl DispatchedActorMessageContext {
    pub fn of(envelope: MessageEnvelope) -> Self {
        Self {
            envelope,
            #[cfg(feature = "tracing")]
            span: tracing::Span::current(),
        }
    }

    /// Decompose the context into the envelope and the handler span.
    pub fn into_parts(self) -> (MessageEnvelope, HandlerSpan) {
        (
            self.envelope,
            HandlerSpan {
                #[cfg(feature = "tracing")]
                span: self.span,
            },
        )
    }
}

// SAFETY: The span is `Send` on its own; the envelope's sendability is guaranteed
//         by the same boundary contracts as `DispatchedActorMessage`.
unsafe impl Send for DispatchedActorMessageContext {}

/// The tracing span a handler future runs under.
///
/// This is a `cfg`-shaped wrapper so dispatcher declaration macros don't have to
/// care whether the `tracing` feature is enabled.
pub struct HandlerSpan {
    #[cfg(feature = "tracing")]
    span: tracing::Span,
}

#[cfg(feature = "tracing")]
impl HandlerSpan {
    /// Instrument the handler future with a span parented to the send site.
    pub fn instrument<F: Future>(self, fut: F) -> impl Future<Output = F::Output> {
        use tracing::Instrument;

        fut.instrument(tracing::trace_span!(
            // TODO: More instrumentation here
            parent: &self.span,
            "message_handler",
        ))
    }
}

#[cfg(not(feature = "tracing"))]
impl HandlerSpan {
    /// Instrument the handler future with a span parented to the send site.
    ///
    /// No-op without the `tracing` feature.
    pub fn instrument<F: Future>(self, fut: F) -> F {
        fut
    }
}

/// Future wrapper that unsafely asserts the inner future is `Send`.
///
/// This is the tool run loops use to reclaim the demand proof that boxing into
/// [`BoxedHandlerFuture`] erased: dispatchers are checked against the loop's
/// [`crate::actor::DispatchDemand`] at their declaration sites, but the erased
/// box type carries no `Send` bound.
///
/// # Safety
/// Constructing this is `unsafe`: the caller asserts the inner future is in fact
/// safe to send between threads. For erased dispatch futures of an actor whose
/// run loop demands [`crate::actor::ThreadSafe`], this is anchored by:
/// - statically declared dispatchers being demand-checked at declaration
///   ([`declare_static_dispatcher!`](crate::declare_static_dispatcher)),
/// - dynamically bound dispatchers being covered by the
///   [`crate::actor::ActorRuntimeBinder`] contract, and
/// - [`DispatchedActorMessage::new`] being unsafe with the same obligation,
///   so safe code cannot forge unchecked deliveries.
#[pin_project::pin_project]
pub struct AssertSend<F> {
    #[pin]
    inner: F,
}

impl<F> AssertSend<F> {
    /// Assert that `inner` is safe to send between threads.
    ///
    /// # Safety
    /// See the type-level documentation.
    pub const unsafe fn new(inner: F) -> Self {
        Self { inner }
    }
}

// SAFETY: Asserted by the caller of `AssertSend::new`.
unsafe impl<F> Send for AssertSend<F> {}

impl<F: Future> Future for AssertSend<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().inner.poll(cx)
    }
}
