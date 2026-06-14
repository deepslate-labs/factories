use crate::actor::work::{ErasedWork, WorkConverter};
use crate::message::envelope::MessageEnvelope;
use core::marker::PhantomData;

/// The function a dispatcher casts the erased context back through.
pub type ActorMessageDispatcherHandler = for<'ctx> unsafe fn(
    DispatchContextPtr<'ctx>,
    DispatchedActorMessageContext,
) -> ErasedWork<'ctx>;

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

    /// Invoke the dispatcher, producing the [`ErasedWork`] that acquires the
    /// lock and runs the handler. The run loop drives this work to completion.
    ///
    /// # Safety
    /// - `dispatch_context` must have been constructed from a reference to the
    ///   dispatch context of the actor type this dispatcher was bound for.
    /// - The envelope in `message_context` must carry a message of the type this
    ///   dispatcher was bound for.
    /// - The caller must invoke this on the actor's thread.
    pub(crate) unsafe fn invoke(
        self,
        dispatch_context: DispatchContextPtr,
        message_context: DispatchedActorMessageContext,
    ) -> ErasedWork {
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

// SAFETY: Same as above - a fn pointer is trivially Sync.
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
    /// - erases the dispatch through `A`'s run loop's
    ///   [`WorkConverter`](crate::actor::ActorRunLoop::WorkConverter).
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
/// Expands to a dispatcher that folds lock acquisition and the handler into one
/// acquire-then-handle composite and erases it through the actor's run loop's
/// [`WorkConverter`](crate::actor::ActorRunLoop::WorkConverter) *at this
/// declaration site*, where the concrete types are known. The converter's
/// requirement is checked here: under a work-stealing loop's
/// [`SendFutureConverter`](crate::actor::work::SendFutureConverter) a handler
/// that produces a `!Send` future (or a `!Send` lock guard) fails to compile.
#[macro_export]
macro_rules! declare_static_dispatcher {
    ($actor:ty, $message:ty) => {{
        unsafe fn invoke<'ctx>(
            dispatch_context: $crate::actor::dispatch::DispatchContextPtr<'ctx>,
            message_context: $crate::actor::dispatch::DispatchedActorMessageContext,
        ) -> $crate::actor::work::ErasedWork<'ctx> {
            type RunLoop = <$actor as $crate::actor::Actor>::RunLoop;
            type Converter = <RunLoop as $crate::actor::ActorRunLoop<$actor>>::WorkConverter;
            type Mode = <$actor as $crate::actor::MessageHandler<$message>>::AccessMode;

            // SAFETY: The `StaticDispatcher` contract guarantees this dispatcher is
            //         only invoked with the dispatch context of `$actor`'s run loop.
            let dispatch_context = unsafe {
                dispatch_context
                    .cast_as::<<RunLoop as $crate::actor::ActorRunLoop<$actor>>::DispatchContext>()
            };

            // The whole dispatch as one future: acquire the lock, build the
            // handler context, run the handler's work to completion.
            let composite = async move {
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

                // `handle` returns this loop's `IntoRunLoopWork`; erase it through
                // the loop's converter and drive it. The converter's `Erased` is a
                // future here (the standard loops' protocol), so awaiting it is the
                // drive; a non-future converter simply can't reach this macro.
                let work = <$actor as $crate::actor::MessageHandler<$message>>::handle(ctx);
                let handler = $crate::actor::work::into_work::<Converter, _>(work);
                span.instrument(handler).await;
            };

            // Erase the acquire-then-handle composite through the same converter
            // (proving its `Send`-ness here, concretely), then pack it into the
            // opaque cell that crosses the uniform dispatcher fn-pointer.
            let erased = $crate::actor::work::into_work::<Converter, _>(composite);
            $crate::actor::work::ErasedWork::pack(erased)
        }

        // SAFETY: `invoke` above is generated for exactly this actor/message pair
        //         and erased through the actor's run loop's converter.
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
    /// actor this message will be delivered to, erasing it through that actor's
    /// run loop's [`WorkConverter`](crate::actor::ActorRunLoop::WorkConverter).
    /// Dispatchers obtained from [`crate::actor::MessageHandler::DISPATCHER`] or a
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

    /// Dispatch the message onto the actor loop, yielding the run loop's work
    /// (its converter's [`Erased`](crate::actor::work::WorkConverter::Erased)) -
    /// for the standard loops, a `Send` future the loop drives.
    ///
    /// This is the building block for hand-rolled run loops: the opaque work cell
    /// stays internal (built and unpacked here, synchronously), and the caller
    /// gets the converter's typed work with its real bounds intact.
    ///
    /// # Safety
    /// `A` must be the target type the actor message was bound for - the
    /// dispatcher then packed `A`'s converter's `Erased`, so recovering it as that
    /// type is sound. (A mismatched `A` is type confusion.)
    ///
    /// In addition to that must this method be called in an appropriate dispatch context. For most
    /// actors this means in the actor dispatch loop, unless pass through messaging is enabled
    /// for the actor.
    pub unsafe fn dispatch_onto_loop<A: crate::actor::Actor + ?Sized>(
        self,
        dispatch_context: &<A::RunLoop as crate::actor::ActorRunLoop<A>>::DispatchContext,
    ) -> <<A::RunLoop as crate::actor::ActorRunLoop<A>>::WorkConverter as WorkConverter>::Erased<'_>
    {
        let (dispatcher, message_context) = self.into_parts();

        let cell = unsafe {
            dispatcher.invoke(DispatchContextPtr::new(dispatch_context), message_context)
        };

        // SAFETY: the dispatcher was bound for `A` (this method's contract), so it
        //         packed `A`'s run loop's converter's `Erased` into the cell.
        unsafe { cell.unpack() }
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
