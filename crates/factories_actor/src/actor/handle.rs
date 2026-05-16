use crate::actor::dispatch::{
    ActorMessageDispatcher, DispatchedActorMessage, DispatchedActorMessageContext,
};
use crate::actor::identity::{ActorIdentity, AnyActorIdentity};
use crate::actor::rtti::ActorRtti;
use crate::actor::{
    Actor, ActorChannel, ActorChannelSendError, ActorChannelSendable, DynActorChannelSendable,
    MessageHandler,
};
use crate::message::Message;
use crate::message::channel::{AnswerReceiver, AnswerSender};
use crate::message::envelope::MessageEnvelope;
use crate::message::rtti::MessageRtti;
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::marker::PhantomData;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct TypedActorHandle<A: Actor>(Arc<ActorIdentity<A>>);

impl<A: Actor> TypedActorHandle<A> {
    /// Type erase the actor handle into an untyped local handle.
    pub fn erase_type_local(self) -> AnyLocalActorHandle
    where
        A: 'static,
    {
        let this = self.0 as Arc<dyn AnyActorIdentity>;
        AnyLocalActorHandle(this)
    }

    /// Type erase the actor handle into an untyped handle.
    pub fn erase_type(self) -> AnyActorHandle
    where
        A: 'static,
        <A as Actor>::Channel: Send + Sync,
    {
        let this = self.0 as Arc<dyn AnyActorIdentity + Send + Sync>;
        AnyActorHandle(this)
    }

    /// Retrieve the channel of the actor.
    pub fn channel(&self) -> &A::Channel {
        &self.0.channel
    }

    /// Prepare sending a statically dispatched message to this actor.
    pub fn prepare_send<M: Message>(
        &self,
        message: M,
        answer_sender: Option<AnswerSender<M>>,
    ) -> impl ActorChannelSendable<'_>
    where
        A: MessageHandler<M>,
    {
        let dispatcher = ActorMessageDispatcher::bind_static::<A, M>();

        ActorChannel::prepare_send(
            self.channel(),
            DispatchedActorMessage::new(
                dispatcher,
                DispatchedActorMessageContext::of(MessageEnvelope::new(message, answer_sender)),
            ),
        )
    }

    /// Send a message to the actor without expecting a reply.
    pub fn tell<M: Message>(&self, message: M) -> impl ActorChannelSendable<'_>
    where
        A: MessageHandler<M>,
    {
        self.prepare_send(message, None)
    }

    /// Send a message to the actor expecting a reply.
    #[cfg(feature = "tokio")]
    pub fn ask<M: Message>(&self, message: M) -> AskSendable<'_, M, impl ActorChannelSendable<'_>>
    where
        A: MessageHandler<M>,
    {
        let (answer_sender, answer_receiver) = crate::message::channel::answer_channel();

        let sendable = self.prepare_send(message, Some(answer_sender));

        AskSendable {
            sendable,
            receive: answer_receiver,
            _data: PhantomData,
        }
    }
}

pub struct AskSendable<'a, M: Message, S: ActorChannelSendable<'a>> {
    sendable: S,
    receive: AnswerReceiver<M>,
    _data: PhantomData<&'a M>,
}

impl<'a, M: Message, S: ActorChannelSendable<'a>> AskSendable<'a, M, S> {
    /// Perform the message exchange asynchronously with the actor.
    pub async fn exchange(self) -> Result<M::Answer, AskError> {
        self.sendable.send().await.map_err(|(err, _)| err)?;
        self.receive.recv().await.ok_or(AskError::NoReply)
    }

    /// Perform the message exchange synchronously with the actor.
    pub fn blocking_exchange(self) -> Result<M::Answer, AskError> {
        self.sendable.blocking_send().map_err(|(err, _)| err)?;
        self.receive.blocking_recv().ok_or(AskError::NoReply)
    }
}

unsafe impl<'a, M: Message, S: ActorChannelSendable<'a>> Send for AskSendable<'a, M, S> where M: Send
{}
unsafe impl<'a, M: Message, S: ActorChannelSendable<'a>> Sync for AskSendable<'a, M, S> where M: Sync
{}

#[derive(Debug, Error)]
pub enum AskError {
    #[error(transparent)]
    SendFailed(#[from] ActorChannelSendError),

    #[error("no reply received")]
    NoReply,
}

#[derive(Debug, Clone)]
pub struct AnyLocalActorHandle(Arc<dyn AnyActorIdentity>);

impl AnyLocalActorHandle {
    /// Try to convert this handle into a shared handle.
    ///
    /// This fails if the channel implementation is not Send + Sync.
    pub fn into_shared(self) -> Result<AnyActorHandle, Self> {
        let channel_rtti = self.0.rtti().channel();

        if channel_rtti.is_send() && channel_rtti.is_sync() {
            // SAFETY: We just checked that the channel is Send and Sync.
            let this = unsafe {
                core::mem::transmute::<_, Arc<dyn AnyActorIdentity + Send + Sync>>(self.0)
            };
            Ok(AnyActorHandle(this))
        } else {
            Err(self)
        }
    }
}

impl<A: Actor> From<TypedActorHandle<A>> for AnyLocalActorHandle
where
    A: 'static,
{
    fn from(value: TypedActorHandle<A>) -> Self {
        value.erase_type_local()
    }
}

impl TryFrom<AnyLocalActorHandle> for AnyActorHandle {
    type Error = AnyLocalActorHandle;

    fn try_from(value: AnyLocalActorHandle) -> Result<Self, Self::Error> {
        value.into_shared()
    }
}

#[derive(Debug, Clone)]
pub struct AnyActorHandle(Arc<dyn AnyActorIdentity + Send + Sync + 'static>);

impl AnyActorHandle {
    /// Convert this handle into a local handle.
    pub fn into_local(self) -> AnyLocalActorHandle {
        AnyLocalActorHandle(self.0)
    }
}

impl<A: Actor> From<TypedActorHandle<A>> for AnyActorHandle
where
    A: 'static,
    A::Channel: Send + Sync,
{
    fn from(value: TypedActorHandle<A>) -> Self {
        value.erase_type()
    }
}

impl From<AnyActorHandle> for AnyLocalActorHandle {
    fn from(value: AnyActorHandle) -> Self {
        value.into_local()
    }
}

/// Private trait that allows actor handles to automatically implement all actor handle
/// methods.
trait ActorHandleBase {
    type ActorIdentity: AnyActorIdentity + ?Sized;

    /// Retrieve the identity of the actor handle.
    fn identity(&self) -> &Arc<Self::ActorIdentity>;
}

impl<A: Actor> ActorHandleBase for TypedActorHandle<A> {
    type ActorIdentity = ActorIdentity<A>;

    fn identity(&self) -> &Arc<Self::ActorIdentity> {
        &self.0
    }
}

impl ActorHandleBase for AnyActorHandle {
    type ActorIdentity = dyn AnyActorIdentity + Send + Sync;

    fn identity(&self) -> &Arc<Self::ActorIdentity> {
        &self.0
    }
}

impl ActorHandleBase for AnyLocalActorHandle {
    type ActorIdentity = dyn AnyActorIdentity;

    fn identity(&self) -> &Arc<Self::ActorIdentity> {
        &self.0
    }
}

/// A handle to an actor.
///
/// This defines the operations that can generally be done on an actor handle.
#[allow(private_bounds)] // intentionally sealed via ActorHandleBase
pub trait ActorHandle: ActorHandleBase {
    /// Retrieve the RTTI of the actor this handle wraps.
    fn rtti(&self) -> &ActorRtti {
        self.identity().rtti()
    }

    /// Attempt to downcast this handle into a typed handle.
    ///
    /// This only succeeds if this handle actually points to an actor of
    /// type A.
    fn downcast<A: Actor>(&self) -> Option<TypedActorHandle<A>> {
        if self.rtti() != A::RTTI {
            return None;
        }

        let identity = self.identity().clone();

        // SAFETY: We just checked that the RTTI matches
        let identity = unsafe { Arc::from_raw(Arc::into_raw(identity).cast::<ActorIdentity<A>>()) };
        Some(TypedActorHandle(identity))
    }

    /// Bind the dispatcher for the given message type.
    fn bind_dispatcher(&self, message: &MessageRtti) -> Option<ActorMessageDispatcher> {
        self.identity().bind(message)
    }

    /// Prepare sending an actor message with dynamic dispatch.
    ///
    /// This is a low level method and generally not recommended for general use.
    ///
    /// # Safety
    /// The caller must ensure that the actor message is dispatchable
    /// to the actor on this handle and that the message can be handled
    /// on the actor thread.
    unsafe fn prepare_send_dynamic_dispatched(
        &self,
        message: DispatchedActorMessage,
    ) -> Box<dyn DynActorChannelSendable<'_> + '_> {
        self.identity().dyn_channel().prepare_send(message)
    }

    fn prepare_send_dynamic(
        &self,
        message: MessageEnvelope,
    ) -> Option<Box<dyn DynActorChannelSendable<'_> + '_>> {
        let dispatcher = self.bind_dispatcher(message.rtti())?;

        let dispatched_message =
            DispatchedActorMessage::new(dispatcher, DispatchedActorMessageContext::of(message));

        // SAFETY: We just bound the dispatcher, so the message can be dispatched.
        Some(unsafe { self.prepare_send_dynamic_dispatched(dispatched_message) })
    }
}

impl<T: ActorHandleBase> ActorHandle for T {}
