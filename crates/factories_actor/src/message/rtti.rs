// Re-export general RTTI types so macros can use $crate paths
pub use factories_rtti::{AutorefSpecialized, BasicTypeRtti, CloneRtti, autoref_specialize};

use crate::message::Message;
use crate::message::channel::AnswerSender;
use core::any::TypeId;
use core::num::NonZeroUsize;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Contains all the information required at runtime about a message type.
//
// Do not blindly remove align(8), we use this to guarantee the lower 3 bits of pointers
// to RTTI are zero so we can tag them
#[repr(align(8))]
#[derive(Debug)]
pub struct MessageRtti {
    /// The name of the message
    name: &'static str,

    /// The type id of the message
    type_id: TypeId,

    message_info: BasicTypeRtti,
    message_clone_info: AutorefSpecialized<Option<CloneRtti>>,
    message_send_info: AutorefSpecialized<bool>,
    answer_sender_send_info: AutorefSpecialized<bool>,
    answer_sender_info: BasicTypeRtti,

    /// Runtime-assigned dynamic dispatch ID, zero while unassigned.
    dynamic_dispatch_id: AtomicUsize,
}

impl MessageRtti {
    /// Create a new named RTTI of a message type.
    ///
    /// # Safety
    /// The caller must ensure that all passed RTTI is correctly associated with the message type T.
    pub const unsafe fn new_named<T: Message>(
        name: &'static str,
        message_clone_info: AutorefSpecialized<Option<CloneRtti>>,
        message_send_info: AutorefSpecialized<bool>,
        answer_sender_send_info: AutorefSpecialized<bool>,
    ) -> Self {
        Self {
            name,
            type_id: TypeId::of::<T>(),
            message_info: BasicTypeRtti::new::<T>(),
            message_clone_info,
            message_send_info,
            answer_sender_send_info,
            answer_sender_info: BasicTypeRtti::new::<AnswerSender<T>>(),
            dynamic_dispatch_id: AtomicUsize::new(0),
        }
    }

    /// Retrieve the name of the message type.
    ///
    /// Note that this name isn't necessarily unique, and is mostly intended for debugging purposes.
    /// Use the address of the RTTI information itself to determine its identity.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Retrieve the type id of the message type.
    pub const fn type_id(&self) -> TypeId {
        self.type_id
    }

    /// Retrieve information about the message type.
    pub const fn message_type_info(&self) -> BasicTypeRtti {
        self.message_info
    }

    /// Retrieve information about the message clonability.
    pub fn message_clone_info(&self) -> Option<CloneRtti> {
        self.message_clone_info.resolve()
    }

    /// Check whether the message type implements `Send`.
    pub fn is_send(&self) -> bool {
        self.message_send_info.resolve()
    }

    /// Check whether the answer sender for this message type implements `Send`.
    pub fn is_answer_sender_send(&self) -> bool {
        self.answer_sender_send_info.resolve()
    }

    /// Retrieve information about the answer sender for this message type.
    pub const fn answer_sender_type_info(&self) -> BasicTypeRtti {
        self.answer_sender_info
    }

    /// Retrieve the identity of this RTTI.
    pub fn identity(&self) -> usize {
        core::ptr::from_ref(self).addr()
    }

    /// Retrieve the dynamic dispatch ID assigned to this message type, if any.
    ///
    /// IDs are assigned at runtime by a dispatch registry (e.g. the global
    /// registry of the `dynamic-dispatch` feature) and uniquely identify this
    /// message RTTI within the registry's dispatch tables.
    pub fn dynamic_dispatch_id(&self) -> Option<NonZeroUsize> {
        // Relaxed suffices: consumers reach the dispatch tables keyed by these
        // IDs through their own synchronization (e.g. the registry's once
        // cell), which orders the assignment before any lookup.
        NonZeroUsize::new(self.dynamic_dispatch_id.load(Ordering::Relaxed))
    }

    /// Assign the dynamic dispatch ID of this message type.
    ///
    /// The slot is write-once: a second assignment fails with the already
    /// assigned ID. Zero is reserved as the unassigned sentinel, which the
    /// `NonZeroUsize` ID type rules out by construction.
    ///
    /// # Safety
    /// The caller must ensure that within all lookup structures consuming the
    /// ID, it uniquely identifies this message RTTI - binders index dispatch
    /// tables by it, and a colliding ID makes them bind a dispatcher of a
    /// different message type.
    pub unsafe fn assign_dynamic_dispatch_id(&self, id: NonZeroUsize) -> Result<(), NonZeroUsize> {
        match self.dynamic_dispatch_id.compare_exchange(
            0,
            id.get(),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => Ok(()),
            // The CAS failed against the zero sentinel, so the value is non-zero.
            Err(existing) => {
                Err(NonZeroUsize::new(existing).expect("CAS failure implies non-zero"))
            }
        }
    }
}

impl PartialEq for MessageRtti {
    // MessageRtti is only equal to itself, so we can compare their identities for equality.
    fn eq(&self, other: &Self) -> bool {
        PartialEq::eq(&self.identity(), &other.identity())
    }
}

impl Eq for MessageRtti {}

/// Declare a message RTTI.
///
/// The optional third argument overrides the debug name baked into the RTTI
/// (defaults to the stringified type).
#[macro_export]
macro_rules! declare_message_rtti {
    ($name:ident, $type:ty) => {
        $crate::message::rtti::declare_message_rtti!($name, $type, ::core::stringify!($type));
    };
    ($name:ident, $type:ty, $message_name:expr) => {
        pub const $name: &'static $crate::message::rtti::MessageRtti = const {
            const CLONE_RTTI: $crate::factories_rtti::AutorefSpecialized<Option<$crate::factories_rtti::CloneRtti>> = $crate::factories_rtti::autoref_specialize!(
                $type -> Option<$crate::factories_rtti::CloneRtti> {
                    T @ Clone => Some($crate::factories_rtti::CloneRtti::new::<T>()),
                    _ => None,
                }
            );

            const SEND_INFO: $crate::message::rtti::AutorefSpecialized<bool> = $crate::factories_rtti::autoref_specialize!(
                $type -> bool {
                    Send => true,
                    _ => false,
                }
            );

            const ANSWER_SENDER_SEND_INFO: $crate::factories_rtti::AutorefSpecialized<bool> = $crate::factories_rtti::autoref_specialize!(
                $crate::message::channel::AnswerSender<$type> -> bool {
                    Send => true,
                    _ => false,
                }
            );

            static VALUE: $crate::message::rtti::MessageRtti = unsafe {
                $crate::message::rtti::MessageRtti::new_named::<$type>(
                    $message_name,
                    CLONE_RTTI,
                    SEND_INFO,
                    ANSWER_SENDER_SEND_INFO,
                )
            };
            &VALUE
        };
    };
}

pub use declare_message_rtti;
