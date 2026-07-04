//! Priority routing for multi-lane actor channels.
//!
//! A multi-lane channel keeps one queue per priority lane and, at send time,
//! asks an [`ActorMessagePriorityRouter`] which lane a given message belongs in.
//! The mailbox then drains higher-priority lanes ahead of lower-priority ones,
//! so control-class messages can jump ahead of a backlog of data messages
//! without a separate side channel.

use crate::actor::dispatch::DispatchedActorMessage;

/// Standard trait that decides which lane to use for routing an actor message.
///
/// Lane `0` is the highest priority (drained first); larger indices are lower
/// priority. A router must return an index in `0..SEND_LANES` for the channel it
/// is attached to; an out-of-range index makes the message unroutable (the send
/// fails with [`ActorChannelSendError::Unroutable`](crate::actor::channel::ActorChannelSendError::Unroutable)).
pub trait ActorMessagePriorityRouter {
    /// Determine the priority lane of the dispatched message.
    fn priority(&self, dispatched: &DispatchedActorMessage) -> usize;
}

/// Message priority router that applies no priority to any message.
///
/// Everything lands in lane `0`, degenerating a multi-lane channel to a single
/// FIFO queue. This is the router used by the single-lane convenience
/// constructors.
#[derive(Debug, Default, Copy, Clone)]
pub struct NoPriorityRouter;

impl ActorMessagePriorityRouter for NoPriorityRouter {
    fn priority(&self, _dispatched: &DispatchedActorMessage) -> usize {
        0
    }
}
