use crate::actor::Actor;
use core::any::TypeId;
use factories_rtti::SendSyncRtti;

#[derive(Debug)]
pub struct ActorRtti {
    /// The name of the actor
    name: &'static str,

    /// The type id of the actor
    type_id: TypeId,

    channel_rtti: ChannelRtti,
}

impl ActorRtti {
    /// Create a new actor RTTI.
    ///
    /// # Safety
    /// The caller must ensure that the provided RTTI actually matches the actor types.
    pub const unsafe fn new_named<T: Actor + 'static>(
        name: &'static str,
        channel_rtti: ChannelRtti
    ) -> Self {
        Self {
            name,
            type_id: TypeId::of::<T>(),
            channel_rtti
        }
    }

    /// Retrieve the actors channel RTTI.
    pub const fn channel(&self) -> ChannelRtti {
        self.channel_rtti
    }

    /// Retrieve the identity of this RTTI.
    pub fn identity(&self) -> usize {
        core::ptr::from_ref(self).addr()
    }
}

impl PartialEq for ActorRtti {
    fn eq(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
}

impl Eq for ActorRtti {}

#[derive(Debug, Copy, Clone)]
pub struct ChannelRtti {
    send_sync: SendSyncRtti,
}

impl ChannelRtti {
    /// Create a new channel RTTI.
    pub const fn new(send_sync: SendSyncRtti) -> Self {
        Self { send_sync }
    }

    /// Whether the channel is send.
    pub fn is_send(&self) -> bool {
        self.send_sync.is_send()
    }

    /// Whether the channel is sync.
    pub fn is_sync(&self) -> bool {
        self.send_sync.is_sync()
    }
}

/// Declare an actor RTTI.
#[macro_export]
macro_rules! declare_actor_rtti {
    ($name:ident, $type:ty) => {
        pub const $name: &'static $crate::actor::rtti::Actor = const {
            const CHANNEL_RTTI: $crate::actor::rtti::ChannelRtti = $crate::actor::rtti::ChannelRtti::new(
                $crate::factories_rtti::create_send_sync_rtti!($type)
            );

            static VALUE: $crate::actor::rtti::ActorRtti = unsafe {
                $crate::actor::rtti::ActorRtti::new_named::<$type>(
                    stringify!($type),
                    CHANNEL_RTTI
                )
            };
            &VALUE
        };
    };
}

pub use declare_actor_rtti;
