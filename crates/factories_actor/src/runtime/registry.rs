//! Global registry for dynamically dispatched message handlers.
//!
//! Handlers are registered at binary load time via
//! [`register_dynamic_handler!`](crate::register_dynamic_handler) and collected
//! through [`factories_collect`]. The first [`dispatch_registry`] call freezes
//! the registration set and builds per-actor dispatch tables:
//!
//! - Every registered message gets a globally unique
//!   [dynamic dispatch ID](MessageRtti::dynamic_dispatch_id) assigned to its
//!   RTTI.
//! - Messages handled by exactly one actor type get *consecutive* IDs per
//!   actor, so binding them is a subtraction and an array index.
//! - Messages handled by multiple actor types land in small per-actor tables
//!   sorted by ID (binary search).
//!
//! [`RegistryBinder`] resolves its actor's table once at construction (i.e. at
//! spawn time).
//!
//! Registrations that happen after the registry was built (e.g. dynamically
//! loaded libraries) are not part of the tables - their messages never receive
//! a dispatch ID and fail to bind. [`RegistryBinder`] construction panics on
//! such a stale registry in debug builds; release builds can check manually
//! via [`DispatchRegistry::is_stale`].

use crate::actor::dispatch::ActorMessageDispatcher;
use crate::actor::rtti::ActorRtti;
use crate::actor::{Actor, ActorRuntimeBinder, MessageHandler};
use crate::message::Message;
use crate::message::rtti::MessageRtti;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::fmt::{Debug, Formatter};
use core::marker::PhantomData;
use core::num::NonZeroUsize;
use factories_collect::GlobalCollection;
use once_cell::sync::OnceCell;

/// All dynamic handler registrations collected at binary load.
///
/// Entries are registered via
/// [`register_dynamic_handler!`](crate::register_dynamic_handler) and consumed
/// by the first [`dispatch_registry`] call.
pub const DYNAMIC_HANDLERS: &GlobalCollection<DynamicHandlerRegistration> =
    factories_collect::global_collection!(DynamicHandlerRegistration);

/// A single collected handler registration: actor type, message type and the
/// dispatcher connecting them.
#[derive(Debug)]
pub struct DynamicHandlerRegistration {
    actor: &'static ActorRtti,
    message: &'static MessageRtti,
    dispatcher: ActorMessageDispatcher,
}

impl DynamicHandlerRegistration {
    /// Create a registration for an actor/message pair from its statically
    /// declared dispatcher.
    pub const fn new<A, M>() -> Self
    where
        M: Message,
        A: MessageHandler<M>,
    {
        Self {
            actor: A::RTTI,
            message: M::RTTI,
            dispatcher: <A as MessageHandler<M>>::DISPATCHER.into_dispatcher(),
        }
    }

    /// Create a registration from a raw dispatcher.
    ///
    /// # Safety
    /// The caller must ensure that the dispatcher dispatches envelopes carrying
    /// messages described by `message` to actors described by `actor`, erasing
    /// the dispatch through that actor's run loop's
    /// [`WorkConverter`](crate::actor::ActorRunLoop::WorkConverter).
    pub const unsafe fn from_raw(
        actor: &'static ActorRtti,
        message: &'static MessageRtti,
        dispatcher: ActorMessageDispatcher,
    ) -> Self {
        Self {
            actor,
            message,
            dispatcher,
        }
    }

    /// The actor type this registration binds to.
    pub const fn actor(&self) -> &'static ActorRtti {
        self.actor
    }

    /// The message type this registration binds.
    pub const fn message(&self) -> &'static MessageRtti {
        self.message
    }

    /// The dispatcher connecting actor and message.
    pub const fn dispatcher(&self) -> ActorMessageDispatcher {
        self.dispatcher
    }
}

/// Register an actor's message handler for dynamic dispatch.
///
/// Place this at module level next to the
/// [`MessageHandler`](crate::actor::MessageHandler) impl:
///
/// ```ignore
/// impl MessageHandler<Greet> for Greeter { /* ... */ }
/// register_dynamic_handler!(Greeter, Greet);
/// ```
///
/// The registered dispatcher is the pair's
/// [`MessageHandler::DISPATCHER`](crate::actor::MessageHandler::DISPATCHER),
/// so everything checked at its declaration site carries over. Registration
/// happens at binary load; the handler becomes bindable once the
/// [`dispatch_registry`](crate::runtime::registry::dispatch_registry) is built.
#[macro_export]
macro_rules! register_dynamic_handler {
    ($actor:ty, $message:ty) => {
        const _: () = {
            static REGISTRATION: $crate::runtime::registry::DynamicHandlerRegistration =
                $crate::runtime::registry::DynamicHandlerRegistration::new::<$actor, $message>();

            static ENTRY: $crate::factories_collect::GlobalCollectionEntry<
                $crate::runtime::registry::DynamicHandlerRegistration,
            > = $crate::factories_collect::GlobalCollectionEntry::new(&REGISTRATION);

            $crate::factories_collect::register_global_collection_entry!(
                $crate::runtime::registry::DYNAMIC_HANDLERS,
                ENTRY
            );
        };
    };
}

pub use register_dynamic_handler;

static REGISTRY: OnceCell<DispatchRegistry> = OnceCell::new();

/// Retrieve the global dispatch registry, building it on first call.
///
/// The first call freezes the collected registrations into dispatch tables and
/// assigns every registered message its
/// [dynamic dispatch ID](MessageRtti::dynamic_dispatch_id). This is a blocking
/// one-time cost; call it explicitly during startup for deterministic timing,
/// otherwise the first [`RegistryBinder`] construction triggers it.
///
/// # Panics
/// Panics when a registered message already carries an externally assigned
/// dispatch ID (see [`MessageRtti::assign_dynamic_dispatch_id`]).
pub fn dispatch_registry() -> &'static DispatchRegistry {
    REGISTRY.get_or_init(|| DispatchRegistry::build(DYNAMIC_HANDLERS.iter()))
}

/// The built form of the global handler registrations.
#[derive(Debug)]
pub struct DispatchRegistry {
    /// Per-actor tables, sorted by actor RTTI identity.
    actors: Box<[ActorDispatchTable]>,

    /// Number of collected registrations at build time, used to detect
    /// registrations that arrived too late to be part of the tables.
    frozen_count: usize,
}

impl DispatchRegistry {
    /// Build a registry from a set of registrations.
    ///
    /// Duplicate registrations of the same actor/message pair collapse into
    /// one; the registration order is irrelevant.
    fn build(registrations: impl Iterator<Item = &'static DynamicHandlerRegistration>) -> Self {
        let mut regs: Vec<&'static DynamicHandlerRegistration> = registrations.collect();
        let frozen_count = regs.len();

        // Deterministic processing order independent of constructor run order.
        regs.sort_by_key(|reg| (reg.actor.identity(), reg.message.identity()));
        regs.dedup_by_key(|reg| (reg.actor.identity(), reg.message.identity()));

        // Count the handling actors per message to classify unique vs. shared.
        let mut handler_count = BTreeMap::<usize, usize>::new();
        for reg in &regs {
            *handler_count.entry(reg.message.identity()).or_insert(0) += 1;
        }

        // IDs start at 1: zero is the unassigned sentinel of the RTTI slot.
        let mut next_id = NonZeroUsize::MIN;

        struct PendingActor {
            actor: &'static ActorRtti,
            unique_base: NonZeroUsize,
            unique: Vec<ActorMessageDispatcher>,
            shared_regs: Vec<&'static DynamicHandlerRegistration>,
        }

        // Pass 1: messages handled by exactly one actor type get consecutive
        // IDs per actor, making their dispatch table a dense array.
        let mut pending = Vec::<PendingActor>::new();
        for actor_regs in regs.chunk_by(|a, b| a.actor.identity() == b.actor.identity()) {
            let mut actor = PendingActor {
                actor: actor_regs[0].actor,
                unique_base: next_id,
                unique: Vec::new(),
                shared_regs: Vec::new(),
            };

            for reg in actor_regs {
                if handler_count[&reg.message.identity()] == 1 {
                    // The dispatcher index in `unique` corresponds to the ID
                    // offset from `unique_base` - assignment and push happen
                    // in lockstep.
                    Self::assign_id(reg.message, next_id);
                    next_id = next_id.checked_add(1).expect("dispatch ID space exhausted");
                    actor.unique.push(reg.dispatcher);
                } else {
                    actor.shared_regs.push(reg);
                }
            }

            pending.push(actor);
        }

        // Pass 2: messages handled by multiple actor types get the remaining
        // IDs, assigned once per message.
        let mut shared_ids = BTreeMap::<usize, NonZeroUsize>::new();
        for actor in &pending {
            for reg in &actor.shared_regs {
                shared_ids.entry(reg.message.identity()).or_insert_with(|| {
                    let id = next_id;
                    next_id = next_id.checked_add(1).expect("dispatch ID space exhausted");
                    Self::assign_id(reg.message, id);
                    id
                });
            }
        }

        let actors = pending
            .into_iter()
            .map(|actor| {
                let mut shared: Vec<(NonZeroUsize, ActorMessageDispatcher)> = actor
                    .shared_regs
                    .iter()
                    .map(|reg| (shared_ids[&reg.message.identity()], reg.dispatcher))
                    .collect();
                shared.sort_by_key(|(id, _)| *id);

                ActorDispatchTable {
                    actor: actor.actor,
                    unique_base: actor.unique_base,
                    unique: actor.unique.into_boxed_slice(),
                    shared: shared.into_boxed_slice(),
                }
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();

        // `actors` inherits the by-identity sort of `regs`, which `table_for`
        // relies on for its binary search.
        Self {
            actors,
            frozen_count,
        }
    }

    fn assign_id(message: &'static MessageRtti, id: NonZeroUsize) {
        // SAFETY: `next_id` increments monotonically during the build, so every
        //         ID is assigned to exactly one message RTTI, and the tables
        //         built here are the lookup structures consuming them.
        if let Err(existing) = unsafe { message.assign_dynamic_dispatch_id(id) } {
            panic!(
                "dynamic dispatch ID of message `{}` was already assigned externally \
                 (existing ID {existing}), which conflicts with the global dispatch registry",
                message.name()
            );
        }
    }

    /// Number of collected registrations when this registry was built.
    pub const fn frozen_registration_count(&self) -> usize {
        self.frozen_count
    }

    /// Whether handler registrations were collected after this registry was
    /// built.
    ///
    /// Such registrations are not part of the dispatch tables and never bind
    /// (see the module docs). [`RegistryBinder`] construction checks this in
    /// debug builds; call it manually where late registration is a real
    /// possibility (e.g. after loading a dynamic library).
    pub fn is_stale(&self) -> bool {
        DYNAMIC_HANDLERS.iter().count() > self.frozen_count
    }

    /// Panic with the late registrations by name when the registry is stale.
    ///
    /// Debug-build companion of [`Self::is_stale`]: a late registration would
    /// otherwise just silently fail to bind.
    #[cfg(debug_assertions)]
    fn debug_assert_fresh(&self) {
        use alloc::string::String;

        let current = DYNAMIC_HANDLERS.iter().count();
        if current <= self.frozen_count {
            return;
        }

        // New entries are pushed to the collection head, so the post-freeze
        // registrations are exactly the first `current - frozen` entries.
        let late_count = current - self.frozen_count;
        let mut late = String::new();
        for reg in DYNAMIC_HANDLERS.iter().take(late_count) {
            if !late.is_empty() {
                late.push_str(", ");
            }
            late.push_str(reg.actor().name());
            late.push('/');
            late.push_str(reg.message().name());
        }

        panic!(
            "{late_count} dynamic handler registration(s) were collected after the \
             dispatch registry was frozen and will never bind: {late}. Load dynamic \
             libraries and register handlers before the registry is first used."
        );
    }

    /// Look up the dispatch table of the given actor type.
    ///
    /// Returns `None` if no handlers were registered for the actor.
    pub fn table_for(&self, actor: &ActorRtti) -> Option<&ActorDispatchTable> {
        self.actors
            .binary_search_by_key(&actor.identity(), |table| table.actor.identity())
            .ok()
            .map(|idx| &self.actors[idx])
    }
}

/// The dispatch table of a single actor type.
#[derive(Debug)]
pub struct ActorDispatchTable {
    actor: &'static ActorRtti,

    /// First dynamic dispatch ID of the actor's uniquely handled messages.
    unique_base: NonZeroUsize,

    /// Dispatchers of messages only this actor handles, indexed by
    /// `id - unique_base`.
    unique: Box<[ActorMessageDispatcher]>,

    /// Dispatchers of messages handled by multiple actor types, sorted by ID.
    shared: Box<[(NonZeroUsize, ActorMessageDispatcher)]>,
}

impl ActorDispatchTable {
    /// The actor type this table belongs to.
    pub const fn actor(&self) -> &'static ActorRtti {
        self.actor
    }

    /// Look up the dispatcher for the given message.
    ///
    /// Returns `None` if the message was never registered for this actor.
    pub fn bind(&self, message: &MessageRtti) -> Option<ActorMessageDispatcher> {
        let id = message.dynamic_dispatch_id()?;

        // Unique fast path: this actor's exclusively handled messages occupy
        // the consecutive ID range starting at `unique_base`. IDs are globally
        // unique, so an ID inside the range is necessarily one of them.
        if let Some(idx) = id.get().checked_sub(self.unique_base.get()) {
            if let Some(dispatcher) = self.unique.get(idx) {
                return Some(*dispatcher);
            }
        }

        self.shared
            .binary_search_by_key(&id, |(id, _)| *id)
            .ok()
            .map(|idx| self.shared[idx].1)
    }
}

/// Runtime binder backed by the global dispatch registry.
///
/// The binder resolves its actor's [`ActorDispatchTable`] once at construction,
/// triggering the registry build if it hasn't happened yet. Binding afterwards
/// never touches global state.
pub struct RegistryBinder<A: Actor + ?Sized> {
    /// The actor's dispatch table; `None` if the actor has no registered
    /// dynamic handlers.
    table: Option<&'static ActorDispatchTable>,
    _actor: PhantomData<fn(&A)>,
}

impl<A: Actor + ?Sized> RegistryBinder<A> {
    /// Create a binder for `A`, building the global registry if this is the
    /// first construction.
    ///
    /// # Panics
    /// In debug builds, panics when handlers were registered after the
    /// registry was frozen (see [`DispatchRegistry::is_stale`]).
    pub fn new() -> Self {
        let registry = dispatch_registry();

        #[cfg(debug_assertions)]
        registry.debug_assert_fresh();

        Self {
            table: registry.table_for(A::RTTI),
            _actor: PhantomData,
        }
    }
}

impl<A: Actor + ?Sized> Default for RegistryBinder<A> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A: Actor + ?Sized> Clone for RegistryBinder<A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A: Actor + ?Sized> Copy for RegistryBinder<A> {}

impl<A: Actor + ?Sized> Debug for RegistryBinder<A> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RegistryBinder")
            .field("table", &self.table)
            .finish()
    }
}

// SAFETY: The table is resolved by `A`'s RTTI identity and only ever contains
//         dispatchers registered for `A`: `DynamicHandlerRegistration`'s
//         constructors tie dispatcher, actor and message together and carry
//         the declaration-site demand proof (`MessageHandler::DISPATCHER`) or
//         the equivalent `from_raw` contract. `bind` looks entries up by the
//         message's registry-assigned ID, so type coherence holds per message.
unsafe impl<A: Actor + ?Sized> ActorRuntimeBinder for RegistryBinder<A> {
    fn bind(&self, message: &MessageRtti) -> Option<ActorMessageDispatcher> {
        self.table?.bind(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::channel::{ActorChannel, ActorChannelSendResult, ActorChannelSendable};
    use crate::actor::dispatch::{
        DispatchContextPtr, DispatchedActorMessage, DispatchedActorMessageContext,
    };
    use crate::actor::event::DefaultMailboxDriver;
    use crate::actor::handle::TypedActorHandle;
    use crate::actor::work::{ErasedWork, SendFutureConverter, into_work};
    use crate::actor::{ActorRunLoop, ActorRunLoopDispatchContext, LockStrategy, StaticOnlyBinder};
    use crate::message::envelope::MessageEnvelope;
    use core::sync::atomic::{AtomicUsize, Ordering};

    struct UniqueMsg;
    crate::declare_message!(UniqueMsg, ());

    struct SharedMsg;
    crate::declare_message!(SharedMsg, ());

    struct ForeignMsg;
    crate::declare_message!(ForeignMsg, ());

    struct UnassignedMsg;
    crate::declare_message!(UnassignedMsg, ());

    struct WriteOnceMsg;
    crate::declare_message!(WriteOnceMsg, ());

    static DISPATCH_A_CALLS: AtomicUsize = AtomicUsize::new(0);
    static DISPATCH_B_CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe fn dispatch_a(_: DispatchContextPtr, _: DispatchedActorMessageContext) -> ErasedWork {
        DISPATCH_A_CALLS.fetch_add(1, Ordering::Relaxed);
        ErasedWork::pack(into_work::<SendFutureConverter, _>(async {}))
    }

    unsafe fn dispatch_b(_: DispatchContextPtr, _: DispatchedActorMessageContext) -> ErasedWork {
        DISPATCH_B_CALLS.fetch_add(1, Ordering::Relaxed);
        ErasedWork::pack(into_work::<SendFutureConverter, _>(async {}))
    }

    /// Invoke a bound dispatcher the way a run loop would.
    fn invoke<M: Message>(dispatcher: ActorMessageDispatcher, message: M) {
        let context = TableActorLoopContext;
        let message_context =
            DispatchedActorMessageContext::of(MessageEnvelope::new(message, None));

        // SAFETY: The context is the table actor's real dispatch context type,
        //         the envelope carries the message type the dispatcher was
        //         bound for in the test table, and we stay on this thread.
        let acquire =
            unsafe { dispatcher.invoke(DispatchContextPtr::new(&context), message_context) };

        drop(acquire);
    }

    const fn id(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test IDs are non-zero")
    }

    // Minimal actor whose RTTI fills the table's actor field. It is never
    // spawned or driven; the table lookups don't consult it.

    struct TableActor;

    struct TableActorLock;

    impl LockStrategy<TableActor> for TableActorLock {
        fn into_inner(self) -> TableActor {
            TableActor
        }
    }

    struct TableActorChannel;

    impl ActorChannel for TableActorChannel {
        fn prepare_send(&self, _message: DispatchedActorMessage) -> impl ActorChannelSendable<'_> {
            TableActorSendable
        }
    }

    struct TableActorSendable;

    impl ActorChannelSendable<'_> for TableActorSendable {
        fn send(self) -> impl Future<Output = ActorChannelSendResult> + Send {
            async { unimplemented!("the table test actor cannot be messaged") }
        }

        fn blocking_send(self) -> ActorChannelSendResult {
            unimplemented!("the table test actor cannot be messaged")
        }

        fn try_send(self) -> ActorChannelSendResult {
            unimplemented!("the table test actor cannot be messaged")
        }
    }

    struct TableActorLoop;

    impl ActorRunLoop<TableActor> for TableActorLoop {
        type DispatchContext = TableActorLoopContext;
        type WorkConverter = SendFutureConverter;
    }

    struct TableActorLoopContext;

    impl ActorRunLoopDispatchContext<TableActor> for TableActorLoopContext {
        fn lock_strategy(&self) -> &TableActorLock {
            unimplemented!("the table test actor is never driven")
        }

        fn shared_state(&self) -> &crate::actor::state::SharedActorState<TableActor> {
            unimplemented!("the table test actor is never driven")
        }

        fn self_ref(&self) -> &crate::actor::handle::WeakActorHandle<TableActor> {
            unimplemented!("the table test actor is never driven")
        }
    }

    crate::declare_actor_rtti!(TABLE_ACTOR_RTTI, TableActor);

    // SAFETY: The RTTI is declared for exactly this type.
    unsafe impl Actor for TableActor {
        const RTTI: &'static ActorRtti = TABLE_ACTOR_RTTI;

        type Channel = TableActorChannel;
        type Error = ();
        type RuntimeBinder = StaticOnlyBinder;
        type LockStrategy = TableActorLock;
        type RunLoop = TableActorLoop;
        type TypedHandle = TypedActorHandle<Self>;
        type SharedStateExtension = ();
        type EventDriver = DefaultMailboxDriver;
    }

    #[test]
    fn dynamic_dispatch_id_is_write_once() {
        let rtti = <WriteOnceMsg as Message>::RTTI;

        assert_eq!(rtti.dynamic_dispatch_id(), None);

        // SAFETY: Test-local message type, no other consumer of this ID.
        unsafe {
            assert_eq!(rtti.assign_dynamic_dispatch_id(id(42)), Ok(()));
            assert_eq!(rtti.assign_dynamic_dispatch_id(id(43)), Err(id(42)));
        }

        assert_eq!(rtti.dynamic_dispatch_id(), Some(id(42)));
    }

    #[test]
    fn table_bind_resolves_unique_and_shared() {
        // SAFETY: Test-local message types, the IDs match the test table.
        unsafe {
            let _ = <UniqueMsg as Message>::RTTI.assign_dynamic_dispatch_id(id(10));
            let _ = <SharedMsg as Message>::RTTI.assign_dynamic_dispatch_id(id(12));
            let _ = <ForeignMsg as Message>::RTTI.assign_dynamic_dispatch_id(id(100));
        }

        let table = ActorDispatchTable {
            actor: TABLE_ACTOR_RTTI,
            unique_base: id(10),
            unique: Box::new([ActorMessageDispatcher::new(dispatch_a)]),
            shared: Box::new([(id(12), ActorMessageDispatcher::new(dispatch_b))]),
        };

        let unique = table
            .bind(<UniqueMsg as Message>::RTTI)
            .expect("unique message must bind");
        invoke(unique, UniqueMsg);
        assert_eq!(DISPATCH_A_CALLS.load(Ordering::Relaxed), 1);
        assert_eq!(DISPATCH_B_CALLS.load(Ordering::Relaxed), 0);

        let shared = table
            .bind(<SharedMsg as Message>::RTTI)
            .expect("shared message must bind");
        invoke(shared, SharedMsg);
        assert_eq!(DISPATCH_A_CALLS.load(Ordering::Relaxed), 1);
        assert_eq!(DISPATCH_B_CALLS.load(Ordering::Relaxed), 1);

        // Assigned ID, but not in this actor's table.
        assert!(table.bind(<ForeignMsg as Message>::RTTI).is_none());

        // No ID assigned at all.
        assert!(table.bind(<UnassignedMsg as Message>::RTTI).is_none());
    }
}
