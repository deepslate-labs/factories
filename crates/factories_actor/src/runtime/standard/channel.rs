use crate::actor::channel::{ActorChannel, ActorMailbox};

/// Extension of the `ActorChannel` with a few common provided methods.
///
/// This is used by the standard run loop to allow easy creation of actors.
pub trait StandardChannel: ActorChannel + Sized {
    /// Options that can be passed into the creator.
    type CreationOptions;

    /// The mailbox type of this channel.
    type Mailbox: ActorMailbox;

    /// Create the channel with the given options.
    fn create(options: Self::CreationOptions) -> (Self, Self::Mailbox);
}