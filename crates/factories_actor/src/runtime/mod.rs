use crate::actor::dispatch::DispatchedActorMessage;

#[cfg(feature = "kanal-runtime")]
pub mod kanal;

pub mod standard;

/// Standard trait that decides which lane to use for routing an actor message.
pub trait ActorMessagePriorityRouter {
    /// Determine the priority of the dispatched message.
    fn priority(&self, dispatched: &DispatchedActorMessage) -> usize;
}

/// Message priority router that applies no priority to any message.
pub struct NoPriorityRouter;

impl ActorMessagePriorityRouter for NoPriorityRouter {
    fn priority(&self, _dispatched: &DispatchedActorMessage) -> usize {
        0
    }
}