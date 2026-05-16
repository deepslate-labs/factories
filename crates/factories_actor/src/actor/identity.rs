use crate::actor::dispatch::ActorMessageDispatcher;
use crate::actor::rtti::ActorRtti;
use crate::actor::{Actor, ActorRuntimeBinder, DynActorChannel};
use crate::message::rtti::MessageRtti;
use core::fmt::{Debug, Formatter};

pub(crate) struct ActorIdentity<A: Actor> {
    pub rtti: &'static ActorRtti,
    pub channel: A::Channel,
    pub binder: A::RuntimeBinder,
}

impl<A: Actor> ActorIdentity<A> {
    /// Create a new actor identity pointing to the given channel.
    pub const fn new(channel: A::Channel, binder: A::RuntimeBinder) -> Self {
        Self {
            rtti: A::RTTI,
            channel,
            binder,
        }
    }
}

pub(crate) trait AnyActorIdentity: Debug {
    /// Retrieve the RTTI this actor identity is associated with.
    fn rtti(&self) -> &'static ActorRtti;

    /// Bind the dispatcher for the given message type.
    fn bind(&self, message: &MessageRtti) -> Option<ActorMessageDispatcher>;

    /// Retrieve the dynamic dispatched channel.
    fn dyn_channel(&self) -> &dyn DynActorChannel;
}

impl<A: Actor> AnyActorIdentity for ActorIdentity<A> {
    fn rtti(&self) -> &'static ActorRtti {
        &self.rtti
    }

    fn bind(&self, message: &MessageRtti) -> Option<ActorMessageDispatcher> {
        self.binder.bind(message)
    }

    fn dyn_channel(&self) -> &dyn DynActorChannel {
        &self.channel
    }
}

impl<A: Actor> Debug for ActorIdentity<A> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ActorIdentity")
            .field("rtti", &(&raw const self.rtti).addr())
            .finish()
    }
}
