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
    error_rtti: SendSyncRtti,
}

impl ActorRtti {
    /// Create a new actor RTTI.
    ///
    /// # Safety
    /// The caller must ensure that the provided RTTI actually matches the actor types.
    pub const unsafe fn new_named<T: Actor + 'static>(
        name: &'static str,
        channel_rtti: ChannelRtti,
        error_rtti: SendSyncRtti,
    ) -> Self {
        Self {
            name,
            type_id: TypeId::of::<T>(),
            channel_rtti,
            error_rtti,
        }
    }

    /// Retrieve the name of the actor type.
    ///
    /// Note that this name isn't necessarily unique, and is mostly intended for
    /// debugging purposes. Use the address of the RTTI information itself to
    /// determine its identity.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Retrieve the type id of the actor type.
    pub const fn type_id(&self) -> TypeId {
        self.type_id
    }

    /// Retrieve the actors channel RTTI.
    pub const fn channel(&self) -> ChannelRtti {
        self.channel_rtti
    }

    /// Retrieve the thread-safety RTTI of the actor's error type.
    pub const fn error(&self) -> SendSyncRtti {
        self.error_rtti
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
        pub const $name: &'static $crate::actor::rtti::ActorRtti = const {
            const CHANNEL_RTTI: $crate::actor::rtti::ChannelRtti =
                $crate::actor::rtti::ChannelRtti::new($crate::factories_rtti::create_send_sync_rtti!(
                    <$type as $crate::actor::Actor>::Channel
                ));

            const ERROR_RTTI: $crate::factories_rtti::SendSyncRtti =
                $crate::factories_rtti::create_send_sync_rtti!(<$type as $crate::actor::Actor>::Error);

            static VALUE: $crate::actor::rtti::ActorRtti = unsafe {
                $crate::actor::rtti::ActorRtti::new_named::<$type>(
                    stringify!($type),
                    CHANNEL_RTTI,
                    ERROR_RTTI,
                )
            };
            &VALUE
        };
    };
}

pub use declare_actor_rtti;
