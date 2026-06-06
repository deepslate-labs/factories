use crate::actor::channel::DynActorChannel;
use crate::actor::dispatch::ActorMessageDispatcher;
use crate::actor::rtti::ActorRtti;
use crate::actor::state::SharedActorState;
use crate::actor::task::ActorTaskHandle;
use crate::actor::{Actor, ActorRuntimeBinder};
use crate::message::rtti::MessageRtti;
use core::fmt::{Debug, Formatter};

pub(crate) struct ActorIdentity<A: Actor + ?Sized> {
    pub rtti: &'static ActorRtti,
    pub channel: A::Channel,
    pub binder: A::RuntimeBinder,
    pub shared: SharedActorState<A>,
}

impl<A: Actor + ?Sized> ActorIdentity<A> {
    /// Create a new actor identity pointing to the given channel.
    pub const fn new(
        channel: A::Channel,
        binder: A::RuntimeBinder,
        shared: SharedActorState<A>,
    ) -> Self {
        Self {
            rtti: A::RTTI,
            channel,
            binder,
            shared,
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

    /// Retrieve the task handle of the actor task, if one was attached.
    fn task(&self) -> Option<&ActorTaskHandle>;
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

    fn task(&self) -> Option<&ActorTaskHandle> {
        self.shared.task()
    }
}

impl<A: Actor> Debug for ActorIdentity<A> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ActorIdentity")
            .field("rtti", &(&raw const self.rtti).addr())
            .finish()
    }
}
