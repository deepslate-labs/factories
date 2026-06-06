use crate::message::rtti::MessageRtti;

pub mod rtti;
pub mod channel;
pub mod envelope;

/// Derive macro generating a [`Message`] implementation together with its
/// RTTI declaration. Configured via `#[message(...)]`; the answer type
/// defaults to `()`.
#[cfg(feature = "derive")]
pub use factories_actor_macro::Message;

/// Declares a struct as being an actor message type.
pub unsafe trait Message where Self: Sized + 'static {
    /// The RTTI associated with this message.
    ///
    /// This must match the actual type this trait is implemented on,
    /// otherwise the implementation is unsound.
    const RTTI: &'static MessageRtti;

    /// The answer type of this message.
    type Answer: Sized;
}

/// Declare a type as a message type.
#[macro_export]
macro_rules! declare_message {
    ($name:ident, $answer:ty) => {
        $crate::paste::paste! {
            // SAFETY: The macro generates the correct data automatically
            $crate::message::rtti::declare_message_rtti!([<$name:snake:upper _RTTI>], $name);

            unsafe impl $crate::message::Message for $name {
                const RTTI: &'static $crate::message::rtti::MessageRtti = [<$name:snake:upper _RTTI>];
                type Answer = $answer;
            }
        }
    };
}

pub use declare_message;
