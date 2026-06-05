use crate::actor::{
    AccessMode, Actor, ActorRunLoop, ActorRunLoopDispatchContext, MessageHandler,
    MessageHandlerContext,
};
use crate::message::Message;
use crate::message::envelope::MessageEnvelope;
use alloc::boxed::Box;
use core::marker::PhantomData;
use core::pin::Pin;

/// The handler future returned once lock acquisition has completed.
///
/// Run loops push this into their concurrent work set after awaiting the
/// outer [`BoxedAcquireFuture`].
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

pub struct ActorMessageDispatcher {
    handler: ActorMessageDispatcherHandler,
}

impl ActorMessageDispatcher {
    /// Create a new message dispatcher using the given handler.
    pub const fn new(handler: ActorMessageDispatcherHandler) -> Self {
        Self { handler }
    }

    /// Bind the dispatcher statically based on the actor and message types.
    pub const fn bind_static<A: Actor, M: Message>() -> Self
    where
        A: MessageHandler<M>,
    {
        unsafe fn invoke_statically_bound<'ctx, A: Actor, M: Message>(
            dispatch_context: DispatchContextPtr<'ctx>,
            message_context: DispatchedActorMessageContext,
        ) -> BoxedAcquireFuture<'ctx>
        where
            A: MessageHandler<M>,
        {
            // SAFETY: The caller has guaranteed that `dispatch_context` was constructed
            //         from a reference to A's concrete dispatch context type.
            let dispatch_context = unsafe {
                dispatch_context.cast_as::<<A::RunLoop as ActorRunLoop<A>>::DispatchContext>()
            };

            Box::pin(async move {
                let guard = <<A as MessageHandler<M>>::AccessMode as AccessMode<A>>::acquire(
                    dispatch_context.lock_strategy(),
                )
                .await;

                Box::pin(async move {
                    // SAFETY: The caller has guaranteed that `message_context.envelope`
                    //         carries a message of type M.
                    let ctx = unsafe {
                        MessageHandlerContext::<M, A, <A as MessageHandler<M>>::AccessMode>::new_unchecked(
                            guard,
                            message_context.envelope,
                        )
                    };

                    let fut = A::handle(ctx);

                    #[cfg(feature = "tracing")]
                    let fut = {
                        use tracing::Instrument;

                        fut.instrument(tracing::trace_span!(
                            // TODO: More instrumentation here
                            parent: &message_context.span,
                            "message_handler",
                        ))
                    };

                    fut.await
                }) as BoxedHandlerFuture<'ctx>
            })
        }

        Self {
            handler: invoke_statically_bound::<A, M>,
        }
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

#[derive(Debug)]
pub struct DispatchedActorMessage {
    dispatcher: ActorMessageDispatcher,
    context: DispatchedActorMessageContext,
}

impl DispatchedActorMessage {
    pub const fn new(
        dispatcher: ActorMessageDispatcher,
        context: DispatchedActorMessageContext,
    ) -> Self {
        Self {
            dispatcher,
            context,
        }
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
    pub unsafe fn dispatch_onto_loop<A: Actor + ?Sized>(
        self,
        dispatch_context: &<A::RunLoop as ActorRunLoop<A>>::DispatchContext,
    ) -> BoxedAcquireFuture<'_> {
        let (dispatcher, message_context) = self.into_parts();

        unsafe { dispatcher.invoke(DispatchContextPtr::new(dispatch_context), message_context) }
    }
}

// SAFETY: We don't make any assumptions in the dispatch message itself about thread safety
//         nor do we touch anything directly without escalating the unsafe to the caller.
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
}
