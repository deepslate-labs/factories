//! Staleness detection lives in its own test binary: the late registration
//! permanently poisons the global registry of the process, which would break
//! unrelated tests sharing the binary.

#![cfg(all(feature = "dynamic-dispatch", debug_assertions))]

use factories_actor::actor::channel::{ActorChannel, ActorChannelSendResult, ActorChannelSendable};
use factories_actor::actor::dispatch::{DispatchedActorMessage, StaticDispatcher};
use factories_actor::actor::event::DefaultMailboxDriver;
use factories_actor::actor::handle::TypedActorHandle;
use factories_actor::actor::rtti::ActorRtti;
use factories_actor::actor::{
    AccessMode, Actor, ActorRunLoop, ActorRunLoopDispatchContext, ActorRuntimeBinder, LockStrategy,
    MessageHandler, MessageHandlerContext, ThreadLocal,
};
use factories_actor::factories_collect::GlobalCollectionEntry;
use factories_actor::message::Message;
use factories_actor::register_dynamic_handler;
use factories_actor::runtime::registry::{
    DYNAMIC_HANDLERS, DynamicHandlerRegistration, RegistryBinder, dispatch_registry,
};
use factories_actor::{declare_actor_rtti, declare_message, declare_static_dispatcher};
// ---------------------------------------------------------------------------
// Minimal actor: never spawned, only used to construct binders. The channel
// and run loop are inert stand-ins.
// ---------------------------------------------------------------------------

struct StaleActor;

struct StaleLock(StaleActor);

impl LockStrategy<StaleActor> for StaleLock {}

struct StaleChannel;

impl ActorChannel for StaleChannel {
    fn prepare_send(&self, _message: DispatchedActorMessage) -> impl ActorChannelSendable<'_> {
        StaleSendable
    }
}

struct StaleSendable;

impl ActorChannelSendable<'_> for StaleSendable {
    fn send(self) -> impl Future<Output = ActorChannelSendResult> + Send {
        async { unimplemented!("the stale test actor cannot be messaged") }
    }

    fn blocking_send(self) -> ActorChannelSendResult {
        unimplemented!("the stale test actor cannot be messaged")
    }
}

struct StaleLoop;

impl ActorRunLoop<StaleActor> for StaleLoop {
    type DispatchContext = StaleLoopContext;
    type Demand = ThreadLocal;
}

struct StaleLoopContext;

impl ActorRunLoopDispatchContext<StaleActor> for StaleLoopContext {
    fn lock_strategy(&self) -> &StaleLock {
        unimplemented!("the stale test actor is never driven")
    }

    fn shared_state(&self) -> &factories_actor::actor::state::SharedActorState<StaleActor> {
        unimplemented!("the stale test actor is never driven")
    }
}

struct ReadAccess;

impl AccessMode<StaleActor> for ReadAccess {
    type Guard<'a> = &'a StaleActor;

    fn acquire<'a>(lock_strategy: &'a StaleLock) -> impl Future<Output = Self::Guard<'a>>
    where
        Self: 'a,
    {
        core::future::ready(&lock_strategy.0)
    }
}

declare_actor_rtti!(STALE_ACTOR_RTTI, StaleActor);

// SAFETY: The RTTI is declared for exactly this type.
unsafe impl Actor for StaleActor {
    const RTTI: &'static ActorRtti = STALE_ACTOR_RTTI;

    type Channel = StaleChannel;
    type Error = core::convert::Infallible;
    type RuntimeBinder = RegistryBinder<StaleActor>;
    type LockStrategy = StaleLock;
    type RunLoop = StaleLoop;
    type TypedHandle = TypedActorHandle<Self>;
    type SharedStateExtension = ();
    type EventDriver = DefaultMailboxDriver;
}

// -- Messages -------------------------------------------------------------------

/// Registered at binary load, part of the frozen registry.
#[derive(Debug)]
struct EarlyMsg;
declare_message!(EarlyMsg, ());

impl MessageHandler<EarlyMsg> for StaleActor {
    type AccessMode = ReadAccess;

    const DISPATCHER: StaticDispatcher<StaleActor, EarlyMsg> =
        declare_static_dispatcher!(StaleActor, EarlyMsg);

    fn handle<'a>(
        ctx: MessageHandlerContext<'a, EarlyMsg, Self, ReadAccess>,
    ) -> impl Future<Output = ()> + 'a {
        async move {
            drop(ctx);
        }
    }
}

register_dynamic_handler!(StaleActor, EarlyMsg);

/// Registered at runtime AFTER the registry is frozen.
#[derive(Debug)]
struct LateMsg;
declare_message!(LateMsg, ());

impl MessageHandler<LateMsg> for StaleActor {
    type AccessMode = ReadAccess;

    const DISPATCHER: StaticDispatcher<StaleActor, LateMsg> =
        declare_static_dispatcher!(StaleActor, LateMsg);

    fn handle<'a>(
        ctx: MessageHandlerContext<'a, LateMsg, Self, ReadAccess>,
    ) -> impl Future<Output = ()> + 'a {
        async move {
            drop(ctx);
        }
    }
}

static LATE_REGISTRATION: DynamicHandlerRegistration =
    DynamicHandlerRegistration::new::<StaleActor, LateMsg>();
static LATE_ENTRY: GlobalCollectionEntry<DynamicHandlerRegistration> =
    GlobalCollectionEntry::new(&LATE_REGISTRATION);

// -- Test -------------------------------------------------------------------------
//
// A single test controls the ordering; parallel tests would race the global
// staleness transition.

#[test]
fn late_registrations_are_detected() {
    // Freeze the registry. The load-time registration is part of it.
    let registry = dispatch_registry();
    assert!(!registry.is_stale());

    let binder = RegistryBinder::<StaleActor>::new();
    assert!(
        binder.bind(<EarlyMsg as Message>::RTTI).is_some(),
        "load-time registration must bind"
    );

    // Simulate a dynamically loaded library registering too late.
    DYNAMIC_HANDLERS.register(&LATE_ENTRY);

    assert!(registry.is_stale());
    assert_eq!(
        <LateMsg as Message>::RTTI.dynamic_dispatch_id(),
        None,
        "late registrations must not receive an ID"
    );

    // Debug builds catch the stale registry at binder construction. Suppress
    // the expected panic output to keep the test log clean.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(|| {
        let _ = RegistryBinder::<StaleActor>::new();
    });
    std::panic::set_hook(hook);

    assert!(
        result.is_err(),
        "constructing a binder against a stale registry must panic in debug builds"
    );
}
