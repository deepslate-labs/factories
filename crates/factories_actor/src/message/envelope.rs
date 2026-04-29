use crate::message::channel::AnswerSender;
use crate::message::Message;
use crate::message::rtti::{BasicTypeRtti, MessageRtti};
use alloc::boxed::Box;
use core::fmt::{Debug, Formatter};
use core::mem::{ManuallyDrop, MaybeUninit};

/// The expected size of an architecture native CPU cache line.
const CACHE_LINE_SIZE: usize = size_of::<usize>() * 8;

/// Trait implemented by the data areas which can store arbitrary types.
///
/// # Safety
/// The type must not assume anything about the data stored inside of it,
/// other than that the size of its bytes is less than or equal to its `PAYLOAD_SIZE`.
///
/// Additionally, `PAYLOAD_SIZE` must at least be the size of a raw pointer.
unsafe trait DataArea
where
    Self: Sized,
{
    /// The amount of bytes this data area can hold.
    ///
    /// Guaranteed to at least be the size of a raw pointer.
    const PAYLOAD_SIZE: usize = size_of::<Self>();

    /// Create a new data area.
    fn new() -> Self;

    /// Convert this data are into a const pointer of type T.
    fn as_ptr<T>(&self) -> *const T {
        core::ptr::from_ref(self).cast()
    }

    /// Convert this data area into a mut pointer of type T.
    fn as_mut<T>(&mut self) -> *mut T {
        core::ptr::from_mut(self).cast()
    }
}

/// The data type that carries a reply sender.
#[repr(C, align(8))]
struct AnswerSenderData(MaybeUninit<[u8; size_of::<usize>()]>);

// SAFETY: Self is a newtype wrapper around a [u8; N] and doesn't assume anything about its contents
unsafe impl DataArea for AnswerSenderData {
    fn new() -> Self {
        Self(MaybeUninit::uninit())
    }
}

/// The type of a message envelope.
///
/// This determines whether the envelope expects an answer or not.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum EnvelopeType {
    /// The envelope is of the tell type.
    ///
    /// This means that no answer is expected.
    Tell,

    /// The envelope is of the ask type.
    ///
    /// This means that an answer is expected.
    Ask,
}

/// Carrier for all message types.
#[repr(C)]
#[cfg_attr(target_pointer_width = "64", repr(align(64)))]
#[cfg_attr(target_pointer_width = "32", repr(align(32)))]
#[cfg_attr(target_pointer_width = "16", repr(align(16)))]
pub struct MessageEnvelope {
    // This is intentionally first so that the data area is at the beginning of the envelope
    // and gets aligned by it.
    payload: MessageEnvelopePayload,
    header: PackedEnvelopeHeader,
}

impl MessageEnvelope {
    /// Create a new message envelope.
    pub fn new<T: Message>(message: T, answer_sender: Option<AnswerSender<T>>) -> Self {
        let header = PackedEnvelopeHeader::new(T::RTTI, answer_sender.is_some());

        // This entire init is a bit verbose... the problem is we can only guarantee alignment
        // once the self type is constructed, so we need to delay initialize the data areas.
        let payload = match &answer_sender {
            None => MessageEnvelopePayload {
                tell: ManuallyDrop::new(MessageEnvelopeTellPayload {
                    data: DataArea::new(),
                }),
            },
            Some(_) => MessageEnvelopePayload {
                ask: ManuallyDrop::new(MessageEnvelopeAskPayload {
                    answer_sender: DataArea::new(),
                    data: DataArea::new(),
                }),
            },
        };

        let mut this = Self { header, payload };

        match answer_sender {
            None => {
                // SAFETY: We initialized to tell in the previous match
                Self::pack_or_box_into(message, &mut unsafe { &mut this.payload.tell }.data);
            }
            Some(answer_sender) => {
                let ask = unsafe { &mut *this.payload.ask };

                Self::pack_or_box_into(answer_sender, &mut ask.answer_sender);
                Self::pack_or_box_into(message, &mut ask.data);
            }
        }

        this
    }

    /// Retrieve the message RTTI of the message carried by this envelope.
    pub fn rtti(&self) -> &'static MessageRtti {
        self.header.rtti()
    }

    /// Determine the type of the envelope.
    pub fn ty(&self) -> EnvelopeType {
        match self.header.expects_answer() {
            false => EnvelopeType::Tell,
            true => EnvelopeType::Ask,
        }
    }

    /// Retrieve the wrapped payload of this envelope as a reference.
    ///
    /// Returns `None` if the payload is of a different type.
    pub fn payload<T: Message>(&self) -> Option<&T> {
        if T::RTTI != self.rtti() {
            core::hint::cold_path();
            return None;
        }

        // SAFETY: We checked that the payload type is T
        Some(unsafe { self.payload_unchecked::<T>() })
    }

    /// Retrieve the wrapped payload of this envelope as a mutable reference.
    ///
    /// Returns `None` if the payload is of a different type.
    pub fn payload_mut<T: Message>(&mut self) -> Option<&mut T> {
        if T::RTTI != self.rtti() {
            core::hint::cold_path();
            return None;
        }

        // SAFETY: We checked that the payload type is T
        Some(unsafe { self.payload_mut_unchecked::<T>() })
    }

    /// Retrieve the wrapped payload of this envelope as a reference.
    ///
    /// # Safety
    /// The caller must ensure that the payload type of this envelope is T.
    pub unsafe fn payload_unchecked<T: Message>(&self) -> &T {
        debug_assert_eq!(
            T::RTTI,
            self.rtti(),
            "The provided type T must match the message type of this envelope"
        );

        match self.ty() {
            EnvelopeType::Tell => {
                // SAFETY: We just check that we are a tell type and the
                //         caller has ensured that the data area contains a T or boxed T.
                unsafe { Self::unwrap_payload_ref(&self.payload.tell.data) }
            }
            EnvelopeType::Ask => {
                // SAFETY: We just checked that we are an ask type and the
                //         caller has ensured that the data area contains a T or boxed T.
                unsafe { Self::unwrap_payload_ref(&self.payload.ask.data) }
            }
        }
    }

    /// Retrieve the wrapped payload of this envelope as a mutable reference.
    ///
    /// # Safety
    /// The caller must ensure that the payload type of this envelope is T.
    pub unsafe fn payload_mut_unchecked<T: Message>(&mut self) -> &mut T {
        debug_assert_eq!(
            T::RTTI,
            self.rtti(),
            "The provided type T must match the message type of this envelope"
        );

        match self.ty() {
            EnvelopeType::Tell => {
                // SAFETY: We just check that we are a tell type and the
                //         caller has ensured that the data area contains a T or boxed T.
                unsafe { Self::unwrap_payload_mut(&mut (&mut self.payload.tell).data) }
            }
            EnvelopeType::Ask => {
                // SAFETY: We just checked that we are an ask type and the
                //         caller has ensured that the data area contains a T or boxed T.
                unsafe { Self::unwrap_payload_mut(&mut (&mut self.payload.ask).data) }
            }
        }
    }

    /// Unwrap this envelope and reveal its parts.
    ///
    /// Returns `Ok(Message, AnswerSender)` if the payload type is T.
    /// Otherwise unwrapping fails.
    pub fn unwrap<T: Message>(self) -> Result<(T, Option<AnswerSender<T>>), Self> {
        if T::RTTI != self.rtti() {
            core::hint::cold_path();
            return Err(self);
        }

        // SAFETY: We just checked that the payload is type T.
        Ok(unsafe { self.unwrap_unchecked() })
    }

    /// Unwrap this envelope and reveal its parts.
    pub unsafe fn unwrap_unchecked<T: Message>(mut self) -> (T, Option<AnswerSender<T>>) {
        debug_assert_eq!(
            T::RTTI,
            self.rtti(),
            "The provided type T must match the message type of this envelope"
        );

        match self.ty() {
            EnvelopeType::Tell => {
                // SAFETY: We checked that we are of the tell type
                let tell = unsafe { &mut *(&mut self.payload.tell) };

                // SAFETY: The caller has ensured that the payload is of type T
                let message = unsafe { Self::unpack_or_unbox(&tell.data) };

                core::mem::forget(self);

                (message, None)
            }
            EnvelopeType::Ask => {
                // SAFETY: We checked that we are of the ask type
                let ask = unsafe { &mut *(&mut self.payload.ask) };

                // SAFETY: The caller has ensured that the payload is of type T
                let (message, answer_sender) = unsafe {
                    (
                        Self::unpack_or_unbox(&ask.data),
                        Some(Self::unpack_or_unbox(&ask.answer_sender)),
                    )
                };

                core::mem::forget(self);

                (message, answer_sender)
            }
        }
    }

    /// Check whether this envelope can be safely sent across thread boundaries.
    ///
    /// For tell messages, only the message itself needs to be `Send`.
    /// For ask messages, both the message and the answer sender must be `Send`.
    pub fn is_sendable(&self) -> bool {
        if !self.rtti().is_send() {
            return false;
        }

        match self.ty() {
            EnvelopeType::Tell => true,
            EnvelopeType::Ask => self.rtti().is_answer_sender_send(),
        }
    }

    /// Try cloning the envelope.
    ///
    /// Whether the clone succeeds or not depends on the contained message type and whether it
    /// is clonable and if this message is a tell message.
    ///
    /// Ask messages are never clonable due to expecting an answer (and one message can only
    /// receive one answer).
    pub fn try_clone(&self) -> Option<Self> {
        if !matches!(self.ty(), EnvelopeType::Tell) {
            // Ask message, can't clone the response sender
            return None;
        }

        self.try_clone_as_tell()
    }

    /// Try cloning the envelope as a tell message.
    ///
    /// Whether the clone succeeds or not depends on the contained message type and whether it
    /// is clonable.
    ///
    /// If this message is an ask message, it effectively decomposes into a new tell message.
    pub fn try_clone_as_tell(&self) -> Option<Self> {
        let clone_info = self.rtti().message_clone_info()?;

        let (payload_ptr, _) = self.payload_ptr();
        let message_size = self.rtti().message_type_info().size();

        let new_header = PackedEnvelopeHeader::new(self.rtti(), false);
        let new_payload = MessageEnvelopePayload {
            tell: ManuallyDrop::new(MessageEnvelopeTellPayload {
                data: DataArea::new(),
            }),
        };

        let mut new_envelope = Self {
            header: new_header,
            payload: new_payload,
        };

        // SAFETY: We constructed the envelope with a tell payload
        unsafe {
            let data = &mut (&mut new_envelope.payload.tell).data;

            if Self::payload_size(data) < message_size {
                // Need boxed
                let new_boxed = clone_info.clone_into_box(payload_ptr);
                data.as_mut::<*const core::ffi::c_void>().write(new_boxed);
            } else {
                // Can clone directly into the data area
                clone_info.clone_into(payload_ptr, data.as_mut(), false);
            }
        }

        Some(new_envelope)
    }

    /// Retrieve a pointer to the payload.
    ///
    /// This additionally also returns whether the pointer points to a boxed heap instance
    /// created using `Box::into_raw`.
    fn payload_ptr(&self) -> (*const core::ffi::c_void, bool) {
        let message_size = self.rtti().message_type_info().size();

        let (data_ptr, is_boxed) = match self.ty() {
            EnvelopeType::Tell => {
                // SAFETY: We checked that this is a tell message
                let area = unsafe { &self.payload.tell.data };
                (
                    area.as_ptr::<core::ffi::c_void>(),
                    message_size > Self::payload_size(area),
                )
            }
            EnvelopeType::Ask => {
                // SAFETY: We checked that this is an ask message
                let area = unsafe { &self.payload.ask.data };
                (
                    area.as_ptr::<core::ffi::c_void>(),
                    message_size > Self::payload_size(area),
                )
            }
        };

        if is_boxed {
            // SAFETY: The pointer points to the data area, which we know contains a pointer
            //         to the boxed data.
            unsafe { (data_ptr.cast::<*const core::ffi::c_void>().read(), true) }
        } else {
            (data_ptr, false)
        }
    }

    fn pack_or_box_into<T, D: DataArea>(value: T, data_area: &mut D) {
        if size_of::<T>() <= Self::payload_size(data_area) {
            // SAFETY: We checked that T fits into T
            unsafe { data_area.as_mut::<T>().write(value) }
        } else {
            let boxed = Box::into_raw(Box::new(value));

            // SAFETY: boxed is a raw pointer and thus always fits
            unsafe { data_area.as_mut::<*mut T>().write(boxed) }
        }
    }

    unsafe fn unpack_or_unbox<T, D: DataArea>(data_area: &D) -> T {
        if size_of::<T>() <= Self::payload_size(data_area) {
            // SAFETY: The caller has ensured that the data area contains a valid T
            unsafe { data_area.as_ptr::<T>().read() }
        } else {
            // SAFETY: The caller has ensured that the data area contains a valid boxed T
            let boxed_ptr = unsafe { data_area.as_ptr::<*mut T>().read() };

            // SAFETY: boxed_ptr is a valid pointer to a boxed T
            unsafe { *Box::from_raw(boxed_ptr) }
        }
    }

    /// Drop the data in a data area.
    ///
    /// # Safety:
    /// The caller must ensure that the data area was created with `pack_or_box` and that the
    /// type information that is provide is associated with the type `T` on from `pack_or_box`.
    unsafe fn drop_data_area<D: DataArea>(data_area: &mut D, type_information: &BasicTypeRtti) {
        if type_information.size() <= Self::payload_size(data_area) {
            // SAFETY: The caller has ensured that the data area contains an instance of
            //         the type associated with the provided type information.
            unsafe { type_information.drop_in_place(data_area.as_mut()) };
        } else {
            // SAFETY: The caller has ensured that the data area contains a boxed instance
            //         of the type associated with the provided type information.
            unsafe {
                type_information.drop_boxed_ptr(data_area.as_mut::<*mut core::ffi::c_void>().read())
            };
        }
    }

    /// Unwrap the reference that the data area points to.
    ///
    /// # Safety
    /// The caller must ensure that the data area actually contains a
    /// type T or boxed type T.
    unsafe fn unwrap_payload_ref<D: DataArea, T: Message>(data_area: &D) -> &T {
        if size_of::<T>() <= Self::payload_size(data_area) {
            // SAFETY: The caller has ensured that the data area contains a T
            unsafe { &*data_area.as_ptr() }
        } else {
            // SAFETY: The caller has ensured that the data area contains a boxed T
            unsafe { &**data_area.as_ptr::<*const T>() }
        }
    }

    /// Unwrap the mutable reference that the data area points to.
    ///
    /// # Safety
    /// The caller must ensure that the data area actually contains a
    /// type T or boxed type T.
    unsafe fn unwrap_payload_mut<D: DataArea, T: Message>(data_area: &mut D) -> &mut T {
        if size_of::<T>() <= Self::payload_size(data_area) {
            // SAFETY: The caller has ensured that the data area contains a T
            unsafe { &mut *data_area.as_mut() }
        } else {
            // SAFETY: The caller has ensured that the data area contains a boxed T
            unsafe { &mut **data_area.as_mut::<*mut T>() }
        }
    }

    /// Convenience method for retrieving the payload size of a data area.
    ///
    /// This makes it a bit simpler because the rust compiler can infer the type.
    const fn payload_size<D: DataArea>(_data_area: &D) -> usize {
        D::PAYLOAD_SIZE
    }
}

impl Drop for MessageEnvelope {
    fn drop(&mut self) {
        let rtti = self.header.rtti();

        match self.ty() {
            EnvelopeType::Tell => {
                // SAFETY: The data area contains a valid message type
                unsafe {
                    Self::drop_data_area(
                        &mut (&mut self.payload.tell).data,
                        &rtti.message_type_info(),
                    )
                };
            }
            EnvelopeType::Ask => {
                // SAFETY: We checked that this is an "ask" message
                //         and thus the ask field is valid.
                let ask = unsafe { &mut *(&mut self.payload.ask) };

                // SAFETY: The data areas contain valid boxed or direct instances and are valid
                unsafe {
                    Self::drop_data_area(&mut ask.answer_sender, &rtti.answer_sender_type_info());
                    Self::drop_data_area(&mut ask.data, &rtti.message_type_info());
                }
            }
        }
    }
}

impl Debug for MessageEnvelope {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let mut debugger = f.debug_struct("MessageEnvelope");

        debugger.field("type", &self.ty());
        debugger.field("rtti", self.rtti());

        debugger.finish()
    }
}

/// A [`MessageEnvelope`] that is guaranteed to contain a `Send` message.
///
/// This wrapper provides proof that the contained message is safe to send across thread boundaries.
#[repr(transparent)]
#[derive(Debug)]
pub struct SendableEnvelope(MessageEnvelope);

impl SendableEnvelope {
    /// Create a new [`SendableEnvelope`] from a message that is statically known to be `Send`.
    ///
    /// Both the message and the answer sender must be `Send`.
    pub fn new<T: Message + Send>(message: T, answer_sender: Option<AnswerSender<T>>) -> Self
    where
        AnswerSender<T>: Send,
    {
        Self(MessageEnvelope::new(message, answer_sender))
    }

    /// Try to wrap a [`MessageEnvelope`] in a [`SendableEnvelope`].
    ///
    /// Returns `None` if the contained message type does not implement `Send`.
    pub fn try_from_envelope(envelope: MessageEnvelope) -> Result<Self, MessageEnvelope> {
        if envelope.is_sendable() {
            Ok(Self(envelope))
        } else {
            Err(envelope)
        }
    }

    /// Unwrap the [`SendableEnvelope`] into the inner [`MessageEnvelope`].
    pub fn into_inner(self) -> MessageEnvelope {
        self.0
    }

    /// Try cloning the envelope.
    ///
    /// See [`MessageEnvelope::try_clone`] for details. The resulting envelope is
    /// also a [`SendableEnvelope`] since the message type is known to be `Send`.
    pub fn try_clone(&self) -> Option<Self> {
        self.0.try_clone().map(Self)
    }

    /// Try cloning the envelope as a tell message.
    ///
    /// See [`MessageEnvelope::try_clone_as_tell`] for details. The resulting envelope is
    /// also a [`SendableEnvelope`] since the message type is known to be `Send`.
    pub fn try_clone_as_tell(&self) -> Option<Self> {
        self.0.try_clone_as_tell().map(Self)
    }
}

impl core::ops::Deref for SendableEnvelope {
    type Target = MessageEnvelope;

    fn deref(&self) -> &MessageEnvelope {
        &self.0
    }
}

impl core::ops::DerefMut for SendableEnvelope {
    fn deref_mut(&mut self) -> &mut MessageEnvelope {
        &mut self.0
    }
}

impl From<SendableEnvelope> for MessageEnvelope {
    fn from(envelope: SendableEnvelope) -> Self {
        envelope.into_inner()
    }
}

impl TryFrom<MessageEnvelope> for SendableEnvelope {
    type Error = MessageEnvelope;

    fn try_from(envelope: MessageEnvelope) -> Result<Self, Self::Error> {
        Self::try_from_envelope(envelope)
    }
}

// SAFETY: SendableEnvelope can only be constructed when the contained message is Send.
unsafe impl Send for SendableEnvelope {}

/// The payload body of a message envelope.
#[repr(C)]
union MessageEnvelopePayload {
    tell: ManuallyDrop<MessageEnvelopeTellPayload>,
    ask: ManuallyDrop<MessageEnvelopeAskPayload>,
}

/// The data area which the payload is stored in if its a tell payload.
#[repr(C)]
struct MessageEnvelopeTellPayloadData(
    MaybeUninit<[u8; CACHE_LINE_SIZE - size_of::<PackedEnvelopeHeader>()]>,
);

// SAFETY: Self is a newtype wrapper around a [u8; N] and doesn't assume anything about its contents
unsafe impl DataArea for MessageEnvelopeTellPayloadData {
    fn new() -> Self {
        Self(MaybeUninit::uninit())
    }
}

/// Tell payload body where no reply is expected.
#[repr(C)]
struct MessageEnvelopeTellPayload {
    data: MessageEnvelopeTellPayloadData,
}

/// The data area which the payload is stored in if its an ask payload.
#[repr(C)]
struct MessageEnvelopeAskPayloadData(
    MaybeUninit<
        [u8; CACHE_LINE_SIZE - size_of::<PackedEnvelopeHeader>() - size_of::<AnswerSenderData>()],
    >,
);

// SAFETY: Self is a newtype wrapper around a [u8; N] and doesn't assume anything about its contents
unsafe impl DataArea for MessageEnvelopeAskPayloadData {
    fn new() -> Self {
        Self(MaybeUninit::uninit())
    }
}

/// Ask payload body which expects an answer.
#[repr(C)]
struct MessageEnvelopeAskPayload {
    data: MessageEnvelopeAskPayloadData,
    answer_sender: AnswerSenderData,
}

/// Pointer size packed header.
#[repr(C)]
struct PackedEnvelopeHeader(*const core::ffi::c_void);

impl PackedEnvelopeHeader {
    /// Create a new packed envelope header
    pub const fn new(rtti: &'static MessageRtti, with_answer: bool) -> Self {
        // MessageRtti is align(8), so we have the 3 lower bits always zero.
        let rtti_ptr = &raw const *rtti;

        // This is a bit of a hack to make this work in const contexts.
        // We want to tag the lowest bit, but `map_addr`/ptr to usize
        // isn't const. What, however, is const, is `wrapping_byte_add(1)`,
        // which unaligns the pointer and sets the lowest bit.
        //
        // This also preserves provenance and should thus be fine with
        // miri.
        let rtti_ptr = if with_answer {
            rtti_ptr.wrapping_byte_add(1)
        } else {
            rtti_ptr
        };

        Self(rtti_ptr.cast())
    }

    /// Retrieve the RTTI this envelope header points to.
    pub fn rtti(&self) -> &'static MessageRtti {
        let rtti_ptr = self.0.map_addr(|addr| addr & !1).cast();

        unsafe { &*rtti_ptr }
    }

    /// Check whether this envelope expects an answer.
    pub fn expects_answer(&self) -> bool {
        (self.0.addr() & 1) != 0
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::declare_message;
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicUsize, Ordering};

    // -- Test message types ------------------------------------------------

    /// Small message that fits inline in both tell and ask data areas.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct SmallMsg {
        value: u64,
    }
    declare_message!(SmallMsg, ());

    /// Large message that exceeds the data area and must be boxed.
    /// 128 bytes is well above the ~56 byte tell data area on 64-bit.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct LargeMsg {
        data: [u8; 128],
    }
    declare_message!(LargeMsg, ());

    /// Message that tracks drops via a shared counter.
    #[derive(Debug, Clone)]
    struct DropTracker {
        counter: Arc<AtomicUsize>,
    }

    impl Drop for DropTracker {
        fn drop(&mut self) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }
    declare_message!(DropTracker, ());

    /// Small non-clonable message (for testing clone rejection on non-Clone types).
    #[derive(Debug)]
    struct SmallNonClone {
        value: u64,
    }
    declare_message!(SmallNonClone, ());

    /// Non-Send message (raw pointer makes it !Send).
    #[derive(Debug, Clone)]
    struct NonSendMsg {
        ptr: *const (),
    }
    declare_message!(NonSendMsg, ());

    /// Send message with a non-Send answer type.
    #[derive(Debug, Clone)]
    struct SendMsgNonSendAnswer {
        value: u64,
    }
    declare_message!(SendMsgNonSendAnswer, *const ());

    /// Large variant of the drop tracker (forces boxing).
    #[derive(Debug)]
    struct LargeDropTracker {
        counter: Arc<AtomicUsize>,
        _pad: [u8; 128],
    }
    declare_message!(LargeDropTracker, ());

    impl Drop for LargeDropTracker {
        fn drop(&mut self) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn header_size_is_ptr_size() {
        assert_eq!(size_of::<PackedEnvelopeHeader>(), size_of::<usize>());
    }

    #[test]
    fn header_roundtrip_tell() {
        let header = PackedEnvelopeHeader::new(SmallMsg::RTTI, false);
        assert!(!header.expects_answer());
        assert_eq!(header.rtti(), SmallMsg::RTTI);
    }

    #[test]
    fn header_roundtrip_ask() {
        let header = PackedEnvelopeHeader::new(SmallMsg::RTTI, true);
        assert!(header.expects_answer());
        assert_eq!(header.rtti(), SmallMsg::RTTI);
    }

    #[test]
    fn header_rtti_identity_preserved() {
        // The tagged pointer must resolve back to the exact same RTTI static
        let header_tell = PackedEnvelopeHeader::new(SmallMsg::RTTI, false);
        let header_ask = PackedEnvelopeHeader::new(SmallMsg::RTTI, true);
        assert!(core::ptr::eq(header_tell.rtti(), SmallMsg::RTTI));
        assert!(core::ptr::eq(header_ask.rtti(), SmallMsg::RTTI));
    }

    #[test]
    fn header_different_rtti_not_equal() {
        let h1 = PackedEnvelopeHeader::new(SmallMsg::RTTI, false);
        let h2 = PackedEnvelopeHeader::new(LargeMsg::RTTI, false);
        assert_ne!(h1.rtti(), h2.rtti());
    }

    #[test]
    fn envelope_is_cache_line_aligned() {
        assert_eq!(align_of::<MessageEnvelope>(), CACHE_LINE_SIZE);
    }

    #[test]
    fn envelope_fits_in_one_cache_line() {
        assert_eq!(size_of::<MessageEnvelope>(), CACHE_LINE_SIZE);
    }

    #[test]
    fn tell_payload_at_least_pointer_sized() {
        assert!(MessageEnvelopeTellPayloadData::PAYLOAD_SIZE >= size_of::<*const ()>());
    }

    #[test]
    fn ask_payload_at_least_pointer_sized() {
        assert!(MessageEnvelopeAskPayloadData::PAYLOAD_SIZE >= size_of::<*const ()>());
    }

    #[test]
    fn answer_sender_data_at_least_pointer_sized() {
        assert!(AnswerSenderData::PAYLOAD_SIZE >= size_of::<*const ()>());
    }

    #[test]
    fn tell_small_msg_type() {
        let env = MessageEnvelope::new(SmallMsg { value: 1 }, None);
        assert_eq!(env.ty(), EnvelopeType::Tell);
        assert_eq!(env.rtti(), SmallMsg::RTTI);
    }

    #[test]
    fn tell_small_msg_payload_ref() {
        let env = MessageEnvelope::new(SmallMsg { value: 42 }, None);
        let payload = env.payload::<SmallMsg>().expect("type should match");
        assert_eq!(payload.value, 42);
    }

    #[test]
    fn tell_small_msg_payload_mut() {
        let mut env = MessageEnvelope::new(SmallMsg { value: 1 }, None);
        let payload = env.payload_mut::<SmallMsg>().expect("type should match");
        payload.value = 99;
        assert_eq!(env.payload::<SmallMsg>().unwrap().value, 99);
    }

    #[test]
    fn tell_small_msg_unwrap() {
        let env = MessageEnvelope::new(SmallMsg { value: 7 }, None);
        let (msg, sender) = env.unwrap::<SmallMsg>().expect("type should match");
        assert_eq!(msg.value, 7);
        assert!(sender.is_none());
    }

    #[test]
    fn tell_large_msg_type() {
        let env = MessageEnvelope::new(LargeMsg { data: [0xAB; 128] }, None);
        assert_eq!(env.ty(), EnvelopeType::Tell);
        assert_eq!(env.rtti(), LargeMsg::RTTI);
    }

    #[test]
    fn tell_large_msg_payload_ref() {
        let env = MessageEnvelope::new(LargeMsg { data: [0xCD; 128] }, None);
        let payload = env.payload::<LargeMsg>().expect("type should match");
        assert_eq!(payload.data, [0xCD; 128]);
    }

    #[test]
    fn tell_large_msg_payload_mut() {
        let mut env = MessageEnvelope::new(LargeMsg { data: [0; 128] }, None);
        let payload = env.payload_mut::<LargeMsg>().expect("type should match");
        payload.data[0] = 0xFF;
        assert_eq!(env.payload::<LargeMsg>().unwrap().data[0], 0xFF);
    }

    #[test]
    fn tell_large_msg_unwrap() {
        let env = MessageEnvelope::new(LargeMsg { data: [0xEF; 128] }, None);
        let (msg, sender) = env.unwrap::<LargeMsg>().expect("type should match");
        assert_eq!(msg.data, [0xEF; 128]);
        assert!(sender.is_none());
    }

    #[test]
    fn payload_wrong_type_returns_none() {
        let env = MessageEnvelope::new(SmallMsg { value: 1 }, None);
        assert!(env.payload::<LargeMsg>().is_none());
    }

    #[test]
    fn payload_mut_wrong_type_returns_none() {
        let mut env = MessageEnvelope::new(SmallMsg { value: 1 }, None);
        assert!(env.payload_mut::<LargeMsg>().is_none());
    }

    #[test]
    fn unwrap_wrong_type_returns_err() {
        let env = MessageEnvelope::new(SmallMsg { value: 1 }, None);
        let result = env.unwrap::<LargeMsg>();
        assert!(result.is_err());
        // The original envelope is returned back on type mismatch
        let env = result.unwrap_err();
        assert_eq!(env.rtti(), SmallMsg::RTTI);
    }

    #[test]
    fn tell_small_msg_drop_runs() {
        let counter = Arc::new(AtomicUsize::new(0));
        {
            let _env = MessageEnvelope::new(
                DropTracker {
                    counter: counter.clone(),
                },
                None,
            );
        }
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "drop must run exactly once"
        );
    }

    #[test]
    fn tell_large_msg_drop_runs() {
        let counter = Arc::new(AtomicUsize::new(0));
        {
            let _env = MessageEnvelope::new(
                LargeDropTracker {
                    counter: counter.clone(),
                    _pad: [0; 128],
                },
                None,
            );
        }
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "drop must run exactly once for boxed payload"
        );
    }

    #[test]
    fn unwrap_small_msg_does_not_double_drop() {
        let counter = Arc::new(AtomicUsize::new(0));
        let env = MessageEnvelope::new(
            DropTracker {
                counter: counter.clone(),
            },
            None,
        );
        let (msg, _) = env.unwrap::<DropTracker>().unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 0, "unwrap must not drop");
        drop(msg);
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "drop must run exactly once"
        );
    }

    #[test]
    fn unwrap_large_msg_does_not_double_drop() {
        let counter = Arc::new(AtomicUsize::new(0));
        let env = MessageEnvelope::new(
            LargeDropTracker {
                counter: counter.clone(),
                _pad: [0; 128],
            },
            None,
        );
        let (msg, _) = env.unwrap::<LargeDropTracker>().unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 0, "unwrap must not drop");
        drop(msg);
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "drop must run exactly once"
        );
    }

    #[test]
    fn unwrap_type_mismatch_then_drop_runs_once() {
        let counter = Arc::new(AtomicUsize::new(0));
        let env = MessageEnvelope::new(
            DropTracker {
                counter: counter.clone(),
            },
            None,
        );
        // Unwrap with wrong type - should fail and return the envelope
        let env = env.unwrap::<LargeMsg>().unwrap_err();
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "failed unwrap must not drop"
        );
        drop(env);
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "drop must run exactly once"
        );
    }

    #[test]
    fn inline_payload_provenance_survives_ref() {
        let env = MessageEnvelope::new(SmallMsg { value: 0xCAFE }, None);
        let r = env.payload::<SmallMsg>().unwrap();
        // The reference must point inside the envelope itself (inline storage)
        let env_start = core::ptr::from_ref(&env) as usize;
        let env_end = env_start + size_of::<MessageEnvelope>();
        let r_addr = core::ptr::from_ref(r) as usize;
        assert!(
            r_addr >= env_start && r_addr < env_end,
            "inline payload ref should point inside the envelope"
        );
    }

    #[test]
    fn boxed_payload_provenance_survives_ref() {
        let env = MessageEnvelope::new(LargeMsg { data: [0; 128] }, None);
        let r = env.payload::<LargeMsg>().unwrap();
        // The reference must point OUTSIDE the envelope (heap allocated)
        let env_start = core::ptr::from_ref(&env) as usize;
        let env_end = env_start + size_of::<MessageEnvelope>();
        let r_addr = core::ptr::from_ref(r) as usize;
        assert!(
            r_addr < env_start || r_addr >= env_end,
            "boxed payload ref should point outside the envelope (on the heap)"
        );
    }

    #[test]
    fn inline_payload_mut_provenance_survives() {
        let mut env = MessageEnvelope::new(SmallMsg { value: 1 }, None);
        let r = env.payload_mut::<SmallMsg>().unwrap();
        r.value = 123;
        // Re-read to confirm the write went through
        assert_eq!(env.payload::<SmallMsg>().unwrap().value, 123);
    }

    #[test]
    fn boxed_payload_mut_provenance_survives() {
        let mut env = MessageEnvelope::new(LargeMsg { data: [0; 128] }, None);
        let r = env.payload_mut::<LargeMsg>().unwrap();
        r.data[127] = 0xFF;
        assert_eq!(env.payload::<LargeMsg>().unwrap().data[127], 0xFF);
    }

    #[cfg(feature = "tokio")]
    mod ask_tests {
        use super::*;

        #[test]
        fn ask_small_msg_type() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env = MessageEnvelope::new(SmallMsg { value: 5 }, Some(AnswerSender::Tokio(tx)));
            assert_eq!(env.ty(), EnvelopeType::Ask);
            assert_eq!(env.rtti(), SmallMsg::RTTI);
        }

        #[test]
        fn ask_small_msg_payload_ref() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env = MessageEnvelope::new(SmallMsg { value: 77 }, Some(AnswerSender::Tokio(tx)));
            let payload = env.payload::<SmallMsg>().expect("type should match");
            assert_eq!(payload.value, 77);
        }

        #[test]
        fn ask_small_msg_unwrap_returns_sender() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env = MessageEnvelope::new(SmallMsg { value: 10 }, Some(AnswerSender::Tokio(tx)));
            let (msg, sender) = env.unwrap::<SmallMsg>().expect("type should match");
            assert_eq!(msg.value, 10);
            assert!(sender.is_some());
        }

        #[test]
        fn ask_large_msg_payload_ref() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env = MessageEnvelope::new(
                LargeMsg { data: [0xAA; 128] },
                Some(AnswerSender::Tokio(tx)),
            );
            let payload = env.payload::<LargeMsg>().expect("type should match");
            assert_eq!(payload.data, [0xAA; 128]);
        }

        #[test]
        fn ask_large_msg_unwrap_returns_sender() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env = MessageEnvelope::new(
                LargeMsg { data: [0xBB; 128] },
                Some(AnswerSender::Tokio(tx)),
            );
            let (msg, sender) = env.unwrap::<LargeMsg>().expect("type should match");
            assert_eq!(msg.data, [0xBB; 128]);
            assert!(sender.is_some());
        }

        #[test]
        fn ask_drop_runs_for_both_payload_and_sender() {
            let counter = Arc::new(AtomicUsize::new(0));
            let (tx, _rx) = tokio::sync::oneshot::channel();
            {
                let _env = MessageEnvelope::new(
                    DropTracker {
                        counter: counter.clone(),
                    },
                    Some(AnswerSender::Tokio(tx)),
                );
            }
            assert_eq!(
                counter.load(Ordering::SeqCst),
                1,
                "message drop must run exactly once"
            );
        }

        #[test]
        fn ask_unwrap_does_not_double_drop() {
            let counter = Arc::new(AtomicUsize::new(0));
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env = MessageEnvelope::new(
                DropTracker {
                    counter: counter.clone(),
                },
                Some(AnswerSender::Tokio(tx)),
            );
            let (msg, sender) = env.unwrap::<DropTracker>().unwrap();
            assert_eq!(counter.load(Ordering::SeqCst), 0);
            drop(msg);
            assert_eq!(counter.load(Ordering::SeqCst), 1);
            drop(sender);
            assert_eq!(counter.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn ask_large_drop_runs() {
            let counter = Arc::new(AtomicUsize::new(0));
            let (tx, _rx) = tokio::sync::oneshot::channel();
            {
                let _env = MessageEnvelope::new(
                    LargeDropTracker {
                        counter: counter.clone(),
                        _pad: [0; 128],
                    },
                    Some(AnswerSender::Tokio(tx)),
                );
            }
            assert_eq!(counter.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn ask_payload_mut_works() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let mut env =
                MessageEnvelope::new(SmallMsg { value: 0 }, Some(AnswerSender::Tokio(tx)));
            env.payload_mut::<SmallMsg>().unwrap().value = 42;
            assert_eq!(env.payload::<SmallMsg>().unwrap().value, 42);
        }

        #[test]
        fn ask_large_msg_payload_mut() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let mut env =
                MessageEnvelope::new(LargeMsg { data: [0; 128] }, Some(AnswerSender::Tokio(tx)));
            env.payload_mut::<LargeMsg>().unwrap().data[0] = 0xFE;
            assert_eq!(env.payload::<LargeMsg>().unwrap().data[0], 0xFE);
        }

        #[test]
        fn ask_large_unwrap_does_not_double_drop() {
            let counter = Arc::new(AtomicUsize::new(0));
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env = MessageEnvelope::new(
                LargeDropTracker {
                    counter: counter.clone(),
                    _pad: [0; 128],
                },
                Some(AnswerSender::Tokio(tx)),
            );
            let (msg, sender) = env.unwrap::<LargeDropTracker>().unwrap();
            assert_eq!(counter.load(Ordering::SeqCst), 0);
            drop(msg);
            assert_eq!(counter.load(Ordering::SeqCst), 1);
            drop(sender);
            assert_eq!(counter.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn ask_inline_payload_provenance() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env =
                MessageEnvelope::new(SmallMsg { value: 0xBEEF }, Some(AnswerSender::Tokio(tx)));
            let r = env.payload::<SmallMsg>().unwrap();
            let env_start = core::ptr::from_ref(&env) as usize;
            let env_end = env_start + size_of::<MessageEnvelope>();
            let r_addr = core::ptr::from_ref(r) as usize;
            assert!(
                r_addr >= env_start && r_addr < env_end,
                "inline ask payload ref should point inside the envelope"
            );
        }

        #[test]
        fn ask_boxed_payload_provenance() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env =
                MessageEnvelope::new(LargeMsg { data: [0; 128] }, Some(AnswerSender::Tokio(tx)));
            let r = env.payload::<LargeMsg>().unwrap();
            let env_start = core::ptr::from_ref(&env) as usize;
            let env_end = env_start + size_of::<MessageEnvelope>();
            let r_addr = core::ptr::from_ref(r) as usize;
            assert!(
                r_addr < env_start || r_addr >= env_end,
                "boxed ask payload ref should point outside the envelope"
            );
        }

        #[test]
        fn ask_type_mismatch_then_drop() {
            let counter = Arc::new(AtomicUsize::new(0));
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env = MessageEnvelope::new(
                DropTracker {
                    counter: counter.clone(),
                },
                Some(AnswerSender::Tokio(tx)),
            );
            let env = env.unwrap::<LargeMsg>().unwrap_err();
            assert_eq!(counter.load(Ordering::SeqCst), 0);
            drop(env);
            assert_eq!(counter.load(Ordering::SeqCst), 1);
        }
    }

    // -- try_clone / try_clone_as_tell tests --------------------------------

    #[test]
    fn try_clone_tell_small_msg() {
        let env = MessageEnvelope::new(SmallMsg { value: 42 }, None);
        let cloned = env.try_clone().expect("clonable tell should clone");
        assert_eq!(cloned.ty(), EnvelopeType::Tell);
        assert_eq!(cloned.rtti(), SmallMsg::RTTI);
        assert_eq!(cloned.payload::<SmallMsg>().unwrap().value, 42);
    }

    #[test]
    fn try_clone_tell_large_msg() {
        let env = MessageEnvelope::new(LargeMsg { data: [0xAB; 128] }, None);
        let cloned = env.try_clone().expect("clonable tell should clone");
        assert_eq!(cloned.ty(), EnvelopeType::Tell);
        assert_eq!(cloned.payload::<LargeMsg>().unwrap().data, [0xAB; 128]);
    }

    #[test]
    fn try_clone_non_clonable_returns_none() {
        let env = MessageEnvelope::new(SmallNonClone { value: 1 }, None);
        assert!(env.try_clone().is_none());
    }

    #[test]
    fn try_clone_non_clonable_large_returns_none() {
        let env = MessageEnvelope::new(
            LargeDropTracker {
                counter: Arc::new(AtomicUsize::new(0)),
                _pad: [0; 128],
            },
            None,
        );
        assert!(env.try_clone().is_none());
    }

    #[test]
    fn try_clone_as_tell_small_msg() {
        let env = MessageEnvelope::new(SmallMsg { value: 99 }, None);
        let cloned = env.try_clone_as_tell().expect("clonable should clone");
        assert_eq!(cloned.ty(), EnvelopeType::Tell);
        assert_eq!(cloned.payload::<SmallMsg>().unwrap().value, 99);
    }

    #[test]
    fn try_clone_as_tell_large_msg() {
        let env = MessageEnvelope::new(LargeMsg { data: [0xFE; 128] }, None);
        let cloned = env.try_clone_as_tell().expect("clonable should clone");
        assert_eq!(cloned.ty(), EnvelopeType::Tell);
        assert_eq!(cloned.payload::<LargeMsg>().unwrap().data, [0xFE; 128]);
    }

    #[test]
    fn try_clone_as_tell_non_clonable_returns_none() {
        let env = MessageEnvelope::new(SmallNonClone { value: 1 }, None);
        assert!(env.try_clone_as_tell().is_none());
    }

    #[test]
    fn try_clone_is_independent_small() {
        let mut env = MessageEnvelope::new(SmallMsg { value: 10 }, None);
        let cloned = env.try_clone().unwrap();
        env.payload_mut::<SmallMsg>().unwrap().value = 20;
        assert_eq!(cloned.payload::<SmallMsg>().unwrap().value, 10);
        assert_eq!(env.payload::<SmallMsg>().unwrap().value, 20);
    }

    #[test]
    fn try_clone_is_independent_large() {
        let mut env = MessageEnvelope::new(LargeMsg { data: [0; 128] }, None);
        let cloned = env.try_clone().unwrap();
        env.payload_mut::<LargeMsg>().unwrap().data[0] = 0xFF;
        assert_eq!(cloned.payload::<LargeMsg>().unwrap().data[0], 0);
        assert_eq!(env.payload::<LargeMsg>().unwrap().data[0], 0xFF);
    }

    #[test]
    fn try_clone_drop_semantics() {
        let counter = Arc::new(AtomicUsize::new(0));
        let env = MessageEnvelope::new(
            DropTracker {
                counter: counter.clone(),
            },
            None,
        );
        let cloned = env.try_clone().unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 0);
        drop(env);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        drop(cloned);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn try_clone_original_survives() {
        let env = MessageEnvelope::new(SmallMsg { value: 7 }, None);
        let _cloned = env.try_clone().unwrap();
        // Original is still usable
        assert_eq!(env.payload::<SmallMsg>().unwrap().value, 7);
    }

    #[cfg(feature = "tokio")]
    mod try_clone_ask_tests {
        use super::*;

        #[test]
        fn try_clone_ask_returns_none() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env = MessageEnvelope::new(SmallMsg { value: 1 }, Some(AnswerSender::Tokio(tx)));
            assert!(
                env.try_clone().is_none(),
                "ask messages should never be clonable via try_clone"
            );
        }

        #[test]
        fn try_clone_ask_large_returns_none() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env =
                MessageEnvelope::new(LargeMsg { data: [0; 128] }, Some(AnswerSender::Tokio(tx)));
            assert!(env.try_clone().is_none());
        }

        #[test]
        fn try_clone_as_tell_from_ask_small() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env = MessageEnvelope::new(SmallMsg { value: 77 }, Some(AnswerSender::Tokio(tx)));
            let cloned = env
                .try_clone_as_tell()
                .expect("clonable ask should decompose into tell");
            assert_eq!(cloned.ty(), EnvelopeType::Tell);
            assert_eq!(cloned.payload::<SmallMsg>().unwrap().value, 77);
        }

        #[test]
        fn try_clone_as_tell_from_ask_large() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env = MessageEnvelope::new(
                LargeMsg { data: [0xCC; 128] },
                Some(AnswerSender::Tokio(tx)),
            );
            let cloned = env
                .try_clone_as_tell()
                .expect("clonable ask should decompose into tell");
            assert_eq!(cloned.ty(), EnvelopeType::Tell);
            assert_eq!(cloned.payload::<LargeMsg>().unwrap().data, [0xCC; 128]);
        }

        #[test]
        fn try_clone_as_tell_from_ask_non_clonable() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env =
                MessageEnvelope::new(SmallNonClone { value: 1 }, Some(AnswerSender::Tokio(tx)));
            assert!(env.try_clone_as_tell().is_none());
        }

        #[test]
        fn try_clone_as_tell_from_ask_drop_semantics() {
            let counter = Arc::new(AtomicUsize::new(0));
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env = MessageEnvelope::new(
                DropTracker {
                    counter: counter.clone(),
                },
                Some(AnswerSender::Tokio(tx)),
            );
            let cloned = env.try_clone_as_tell().unwrap();
            assert_eq!(counter.load(Ordering::SeqCst), 0);
            drop(env);
            assert_eq!(counter.load(Ordering::SeqCst), 1);
            drop(cloned);
            assert_eq!(counter.load(Ordering::SeqCst), 2);
        }
    }

    #[cfg(feature = "tokio")]
    mod sendable_ask_tests {
        use super::*;

        #[test]
        fn ask_send_msg_send_answer_is_sendable() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env = MessageEnvelope::new(SmallMsg { value: 1 }, Some(AnswerSender::Tokio(tx)));
            assert!(env.is_sendable());
        }

        #[test]
        fn ask_send_msg_non_send_answer_is_not_sendable() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env = MessageEnvelope::new(
                SendMsgNonSendAnswer { value: 1 },
                Some(AnswerSender::Tokio(tx)),
            );
            assert!(
                !env.is_sendable(),
                "ask with !Send answer must not be sendable"
            );
        }

        #[test]
        fn sendable_envelope_rejects_ask_with_non_send_answer() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env = MessageEnvelope::new(
                SendMsgNonSendAnswer { value: 1 },
                Some(AnswerSender::Tokio(tx)),
            );
            assert!(SendableEnvelope::try_from_envelope(env).is_err());
        }

        #[test]
        fn sendable_envelope_accepts_ask_with_send_answer() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let env = MessageEnvelope::new(SmallMsg { value: 42 }, Some(AnswerSender::Tokio(tx)));
            assert!(SendableEnvelope::try_from_envelope(env).is_ok());
        }
    }

    #[test]
    fn zero_sized_inline_check() {
        // Verify that a ZST would be considered "inline" (size 0 <= PAYLOAD_SIZE)
        assert!(size_of::<()>() <= MessageEnvelopeTellPayloadData::PAYLOAD_SIZE);
    }

    #[test]
    fn envelope_size_constants_consistent() {
        // Tell data area + header = cache line
        assert_eq!(
            MessageEnvelopeTellPayloadData::PAYLOAD_SIZE + size_of::<PackedEnvelopeHeader>(),
            CACHE_LINE_SIZE
        );
        // Ask data area + answer sender data + header = cache line
        assert_eq!(
            MessageEnvelopeAskPayloadData::PAYLOAD_SIZE
                + AnswerSenderData::PAYLOAD_SIZE
                + size_of::<PackedEnvelopeHeader>(),
            CACHE_LINE_SIZE
        );
    }

    /// Confirm that small messages are truly inline (not boxed)
    #[test]
    fn small_msg_is_inline() {
        assert!(
            size_of::<SmallMsg>() <= MessageEnvelopeTellPayloadData::PAYLOAD_SIZE,
            "SmallMsg should fit inline in tell payload"
        );
        assert!(
            size_of::<SmallMsg>() <= MessageEnvelopeAskPayloadData::PAYLOAD_SIZE,
            "SmallMsg should fit inline in ask payload"
        );
    }

    /// Confirm that large messages are truly boxed
    #[test]
    fn large_msg_is_boxed() {
        assert!(
            size_of::<LargeMsg>() > MessageEnvelopeTellPayloadData::PAYLOAD_SIZE,
            "LargeMsg should exceed tell payload"
        );
        assert!(
            size_of::<LargeMsg>() > MessageEnvelopeAskPayloadData::PAYLOAD_SIZE,
            "LargeMsg should exceed ask payload"
        );
    }

    // -- Clone RTTI detection -------------------------------------------------

    #[test]
    fn clonable_message_has_clone_rtti() {
        // SmallMsg derives Clone
        assert!(
            SmallMsg::RTTI.message_clone_info().is_some(),
            "SmallMsg is Clone, so clone info should be Some"
        );
    }

    #[test]
    fn clonable_large_message_has_clone_rtti() {
        assert!(
            LargeMsg::RTTI.message_clone_info().is_some(),
            "LargeMsg is Clone, so clone info should be Some"
        );
    }

    #[test]
    fn non_clonable_message_has_no_clone_rtti() {
        // LargeDropTracker does NOT derive Clone
        assert!(
            LargeDropTracker::RTTI.message_clone_info().is_none(),
            "LargeDropTracker is not Clone, so clone info should be None"
        );
    }

    #[test]
    fn clone_rtti_via_envelope_rtti() {
        let env = MessageEnvelope::new(SmallMsg { value: 1 }, None);
        assert!(env.rtti().message_clone_info().is_some());

        let env = MessageEnvelope::new(
            LargeDropTracker {
                counter: Arc::new(AtomicUsize::new(0)),
                _pad: [0; 128],
            },
            None,
        );
        assert!(env.rtti().message_clone_info().is_none());
    }

    #[test]
    fn clone_rtti_clone_into_inline() {
        let clone_rtti = SmallMsg::RTTI.message_clone_info().unwrap();
        let src = SmallMsg { value: 0xBEEF };
        let mut dest = core::mem::MaybeUninit::<SmallMsg>::uninit();

        unsafe {
            clone_rtti.clone_into(
                core::ptr::from_ref(&src).cast(),
                dest.as_mut_ptr().cast(),
                false,
            );
        }

        let dest = unsafe { dest.assume_init() };
        assert_eq!(dest.value, 0xBEEF);
    }

    #[test]
    fn clone_rtti_clone_into_overwrites_initialized() {
        let clone_rtti = SmallMsg::RTTI.message_clone_info().unwrap();
        let src = SmallMsg { value: 42 };
        let mut dest = SmallMsg { value: 0 };

        unsafe {
            clone_rtti.clone_into(
                core::ptr::from_ref(&src).cast(),
                core::ptr::from_mut(&mut dest).cast(),
                true,
            );
        }

        assert_eq!(dest.value, 42);
    }

    #[test]
    fn clone_rtti_clone_into_drops_previous_when_initialized() {
        let clone_rtti = DropTracker::RTTI.message_clone_info().unwrap();
        let counter_src = Arc::new(AtomicUsize::new(0));
        let counter_dest = Arc::new(AtomicUsize::new(0));

        let src = DropTracker {
            counter: counter_src.clone(),
        };
        let mut dest = DropTracker {
            counter: counter_dest.clone(),
        };

        unsafe {
            clone_rtti.clone_into(
                core::ptr::from_ref(&src).cast(),
                core::ptr::from_mut(&mut dest).cast(),
                true,
            );
        }

        // The old dest should have been dropped (via clone_from)
        // and dest should now hold a clone of src
        assert!(Arc::ptr_eq(&dest.counter, &counter_src));
    }

    #[test]
    fn clone_rtti_clone_into_box() {
        let clone_rtti = LargeMsg::RTTI.message_clone_info().unwrap();
        let src = LargeMsg { data: [0xAB; 128] };

        let boxed_ptr = unsafe { clone_rtti.clone_into_box(core::ptr::from_ref(&src).cast()) };

        let boxed = unsafe { Box::from_raw(boxed_ptr.cast::<LargeMsg>()) };
        assert_eq!(boxed.data, [0xAB; 128]);
    }

    #[test]
    fn clone_rtti_clone_into_box_is_independent() {
        let clone_rtti = SmallMsg::RTTI.message_clone_info().unwrap();
        let src = SmallMsg { value: 99 };

        let boxed_ptr = unsafe { clone_rtti.clone_into_box(core::ptr::from_ref(&src).cast()) };

        let mut boxed = unsafe { Box::from_raw(boxed_ptr.cast::<SmallMsg>()) };
        boxed.value = 0;
        // Original is unaffected
        assert_eq!(src.value, 99);
    }

    #[test]
    fn clone_rtti_clone_drop_tracker_preserves_semantics() {
        let clone_rtti = DropTracker::RTTI.message_clone_info().unwrap();
        let counter = Arc::new(AtomicUsize::new(0));
        let src = DropTracker {
            counter: counter.clone(),
        };

        let boxed_ptr = unsafe { clone_rtti.clone_into_box(core::ptr::from_ref(&src).cast()) };

        let cloned = unsafe { Box::from_raw(boxed_ptr.cast::<DropTracker>()) };

        assert_eq!(counter.load(Ordering::SeqCst), 0);
        drop(src);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        drop(cloned);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    // -- Send RTTI detection --------------------------------------------------

    #[test]
    fn send_message_has_send_rtti() {
        assert!(
            SmallMsg::RTTI.is_send(),
            "SmallMsg is Send, so is_send should be true"
        );
    }

    #[test]
    fn send_large_message_has_send_rtti() {
        assert!(LargeMsg::RTTI.is_send());
    }

    #[test]
    fn non_send_message_has_no_send_rtti() {
        assert!(
            !NonSendMsg::RTTI.is_send(),
            "NonSendMsg is !Send, so is_send should be false"
        );
    }

    #[test]
    fn send_rtti_via_envelope_rtti() {
        let env = MessageEnvelope::new(SmallMsg { value: 1 }, None);
        assert!(env.rtti().is_send());

        let env = MessageEnvelope::new(
            NonSendMsg {
                ptr: core::ptr::null(),
            },
            None,
        );
        assert!(!env.rtti().is_send());
    }

    #[test]
    fn sendable_envelope_from_send_message() {
        let env = MessageEnvelope::new(SmallMsg { value: 42 }, None);
        let sendable = SendableEnvelope::try_from_envelope(env);
        assert!(sendable.is_ok());
    }

    #[test]
    fn sendable_envelope_from_non_send_message() {
        let env = MessageEnvelope::new(
            NonSendMsg {
                ptr: core::ptr::null(),
            },
            None,
        );
        let result = SendableEnvelope::try_from_envelope(env);
        assert!(result.is_err());
    }

    #[test]
    fn sendable_envelope_deref_payload() {
        let env = MessageEnvelope::new(SmallMsg { value: 77 }, None);
        let sendable = SendableEnvelope::try_from_envelope(env).unwrap();
        assert_eq!(sendable.payload::<SmallMsg>().unwrap().value, 77);
    }

    #[test]
    fn sendable_envelope_deref_mut_payload() {
        let env = MessageEnvelope::new(SmallMsg { value: 1 }, None);
        let mut sendable = SendableEnvelope::try_from_envelope(env).unwrap();
        sendable.payload_mut::<SmallMsg>().unwrap().value = 99;
        assert_eq!(sendable.payload::<SmallMsg>().unwrap().value, 99);
    }

    #[test]
    fn sendable_envelope_into_inner() {
        let env = MessageEnvelope::new(SmallMsg { value: 10 }, None);
        let sendable = SendableEnvelope::try_from_envelope(env).unwrap();
        let env = sendable.into_inner();
        assert_eq!(env.payload::<SmallMsg>().unwrap().value, 10);
    }

    #[test]
    fn sendable_envelope_rejected_returns_original() {
        let env = MessageEnvelope::new(
            NonSendMsg {
                ptr: core::ptr::null(),
            },
            None,
        );
        let env = SendableEnvelope::try_from_envelope(env).unwrap_err();
        assert_eq!(env.rtti(), NonSendMsg::RTTI);
    }

    #[test]
    fn sendable_envelope_large_msg() {
        let env = MessageEnvelope::new(LargeMsg { data: [0xAB; 128] }, None);
        let sendable = SendableEnvelope::try_from_envelope(env).unwrap();
        assert_eq!(sendable.payload::<LargeMsg>().unwrap().data, [0xAB; 128]);
    }

    #[test]
    fn sendable_envelope_drop_runs() {
        let counter = Arc::new(AtomicUsize::new(0));
        {
            let env = MessageEnvelope::new(
                DropTracker {
                    counter: counter.clone(),
                },
                None,
            );
            let _sendable = SendableEnvelope::try_from_envelope(env).unwrap();
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn sendable_envelope_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<SendableEnvelope>();
    }

    #[test]
    fn send_msg_non_send_answer_rtti() {
        assert!(
            SendMsgNonSendAnswer::RTTI.is_send(),
            "message itself is Send"
        );
        assert!(
            !SendMsgNonSendAnswer::RTTI.is_answer_sender_send(),
            "answer sender is !Send due to *const () answer"
        );
    }

    #[test]
    fn send_msg_send_answer_rtti() {
        assert!(SmallMsg::RTTI.is_send());
        assert!(SmallMsg::RTTI.is_answer_sender_send());
    }

    #[test]
    fn non_send_msg_is_not_sendable() {
        let env = MessageEnvelope::new(
            NonSendMsg {
                ptr: core::ptr::null(),
            },
            None,
        );
        assert!(!env.is_sendable());
    }

    #[test]
    fn tell_send_msg_non_send_answer_is_sendable() {
        // A tell message doesn't carry an answer sender, so only the message's
        // Send-ness matters.
        let env = MessageEnvelope::new(SendMsgNonSendAnswer { value: 1 }, None);
        assert!(env.is_sendable());
    }

    #[test]
    fn sendable_envelope_accepts_tell_with_non_send_answer() {
        let env = MessageEnvelope::new(SendMsgNonSendAnswer { value: 1 }, None);
        assert!(SendableEnvelope::try_from_envelope(env).is_ok());
    }

    #[test]
    fn sendable_envelope_new_static() {
        let sendable = SendableEnvelope::new(SmallMsg { value: 42 }, None);
        assert_eq!(sendable.payload::<SmallMsg>().unwrap().value, 42);
    }
}
