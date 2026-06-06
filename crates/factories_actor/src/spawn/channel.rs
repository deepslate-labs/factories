use crate::actor::channel::ActorChannel;
use crate::actor::dispatch::DispatchedActorMessage;

/// A channel that can be constructed from options as part of generic actor assembly.
///
/// This is the assembly-side contract for channels: anything implementing it can
/// be used by [`crate::spawn::ActorBuilder`]. Channels that cannot (or do not want
/// to) participate in generic assembly only implement [`ActorChannel`] and get
/// wired up by hand.
pub trait CreatableChannel: ActorChannel + Sized {
    /// Options that can be passed into the creator.
    type CreationOptions;

    /// The mailbox type of this channel.
    type Mailbox: ActorMailbox + Send + 'static;

    /// Create the channel with the given options.
    fn create(options: Self::CreationOptions) -> (Self, Self::Mailbox);
}

/// Standard mailbox interface for an actor channel.
///
/// This is what generic run loops consume. It is not a hard requirement for
/// actor channels: a fully custom run loop may use a completely custom receive
/// mechanism.
pub trait ActorMailbox {
    /// Receive a message from the mailbox.
    ///
    /// Resolves to `None` once the channel is closed and drained.
    fn receive(&mut self) -> impl Future<Output = Option<DispatchedActorMessage>> + Send + '_;
}
