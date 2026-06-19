#![cfg(all(
    feature = "dynamic-dispatch",
    feature = "tokio-runtime",
    feature = "tokio-lock",
    feature = "tokio-answer"
))]

use factories_actor::actor::dispatch::StaticDispatcher;
use factories_actor::actor::event::DefaultMailboxDriver;
use factories_actor::actor::handle::{ActorHandle, TypedActorHandle};
use factories_actor::actor::rtti::ActorRtti;
use factories_actor::actor::{Actor, MessageHandler};
use factories_actor::message::Message;
use factories_actor::message::channel::answer_channel;
use factories_actor::message::envelope::MessageEnvelope;
use factories_actor::register_dynamic_handler;
use factories_actor::runtime::concurrent_loop::ConcurrentRunLoop;
use factories_actor::runtime::lock::{Exclusive, Shared};
use factories_actor::runtime::registry::{RegistryBinder, dispatch_registry};
use factories_actor::runtime::tokio::TokioMpscActorChannel;
use factories_actor::runtime::tokio::{TokioMutexLock, TokioRwLock, TokioTaskSpawner};
use factories_actor::spawn::ActorLauncher;
use factories_actor::{declare_actor_rtti, declare_message, declare_static_async_dispatcher};
// ---------------------------------------------------------------------------
// Calc: handles two unique messages (AddValue, GetValue), one shared message
// (Describe) and one statically-only handled message (Unregistered).
// ---------------------------------------------------------------------------

struct Calc {
    value: u32,
}

declare_actor_rtti!(CALC_RTTI, Calc);

// SAFETY: The RTTI is declared for exactly this type.
unsafe impl Actor for Calc {
    const RTTI: &'static ActorRtti = CALC_RTTI;

    type Channel = TokioMpscActorChannel;
    type Error = core::convert::Infallible;
    type RuntimeBinder = RegistryBinder<Calc>;
    type LockStrategy = TokioMutexLock<Calc>;
    type RunLoop = ConcurrentRunLoop<Calc>;
    type TypedHandle = TypedActorHandle<Self>;
    type SharedData = ();
    type EventDriver = DefaultMailboxDriver;
}

// ---------------------------------------------------------------------------
// Mirror: handles only the shared message (Describe), through a read-write
// lock so the handler can use the `Shared` access mode.
// ---------------------------------------------------------------------------

struct Mirror;

declare_actor_rtti!(MIRROR_RTTI, Mirror);

// SAFETY: The RTTI is declared for exactly this type.
unsafe impl Actor for Mirror {
    const RTTI: &'static ActorRtti = MIRROR_RTTI;

    type Channel = TokioMpscActorChannel;
    type Error = core::convert::Infallible;
    type RuntimeBinder = RegistryBinder<Mirror>;
    type LockStrategy = TokioRwLock<Mirror>;
    type RunLoop = ConcurrentRunLoop<Mirror>;
    type TypedHandle = TypedActorHandle<Self>;
    type SharedData = ();
    type EventDriver = DefaultMailboxDriver;
}

// -- Messages -------------------------------------------------------------------

#[derive(Debug)]
struct AddValue(u32);
declare_message!(AddValue, ());

impl MessageHandler<AddValue> for Calc {
    type AccessMode = Exclusive;

    const DISPATCHER: StaticDispatcher<Calc, AddValue> =
        declare_static_async_dispatcher!(Calc, AddValue, |ctx| async move {
            let (mut guard, message, _) = ctx.into_parts();
            guard.value += message.0;
        });
}

register_dynamic_handler!(Calc, AddValue);
// A duplicate registration must collapse during the registry build.
register_dynamic_handler!(Calc, AddValue);

#[derive(Debug)]
struct GetValue;
declare_message!(GetValue, u32);

impl MessageHandler<GetValue> for Calc {
    type AccessMode = Exclusive;

    const DISPATCHER: StaticDispatcher<Calc, GetValue> =
        declare_static_async_dispatcher!(Calc, GetValue, |ctx| async move {
            let (guard, _, answer) = ctx.into_parts();
            if let Some(answer) = answer {
                let _ = answer.send(guard.value);
            }
        });
}

register_dynamic_handler!(Calc, GetValue);

/// Shared between both actor types: exercises the per-actor shared tables.
#[derive(Debug)]
struct Describe;
declare_message!(Describe, String);

impl MessageHandler<Describe> for Calc {
    type AccessMode = Exclusive;

    const DISPATCHER: StaticDispatcher<Calc, Describe> =
        declare_static_async_dispatcher!(Calc, Describe, |ctx| async move {
            let (guard, _, answer) = ctx.into_parts();
            if let Some(answer) = answer {
                let _ = answer.send(format!("calc({})", guard.value));
            }
        });
}

register_dynamic_handler!(Calc, Describe);

impl MessageHandler<Describe> for Mirror {
    type AccessMode = Shared;

    const DISPATCHER: StaticDispatcher<Mirror, Describe> =
        declare_static_async_dispatcher!(Mirror, Describe, |ctx| async move {
            let (_, _, answer) = ctx.into_parts();
            if let Some(answer) = answer {
                let _ = answer.send("mirror".to_string());
            }
        });
}

register_dynamic_handler!(Mirror, Describe);

/// Statically handled by Calc but never registered for dynamic dispatch.
#[derive(Debug)]
struct Unregistered;
declare_message!(Unregistered, ());

impl MessageHandler<Unregistered> for Calc {
    type AccessMode = Exclusive;

    const DISPATCHER: StaticDispatcher<Calc, Unregistered> =
        declare_static_async_dispatcher!(Calc, Unregistered, |ctx| async move {
            drop(ctx);
        });
}

// -- Helpers --------------------------------------------------------------------

async fn spawn_calc(value: u32) -> TypedActorHandle<Calc> {
    let spawner = TokioTaskSpawner::current();

    ActorLauncher::default()
        .spawn_ready(&spawner, Calc { value })
        .await
        .expect("calc init is infallible")
}

async fn spawn_mirror() -> TypedActorHandle<Mirror> {
    let spawner = TokioTaskSpawner::current();

    ActorLauncher::default()
        .spawn_ready(&spawner, Mirror)
        .await
        .expect("mirror init is infallible")
}

/// Dynamically ask `message` through a type-erased handle.
async fn dyn_ask<M: Message>(handle: &impl ActorHandle, message: M) -> M::Answer {
    let (answer_sender, answer_receiver) = answer_channel::<M>();
    let envelope = MessageEnvelope::new(message, Some(answer_sender));

    handle
        .prepare_send_dynamic(envelope)
        .expect("message must bind")
        .send()
        .await
        .expect("send must succeed");

    answer_receiver.recv().await.expect("answer must arrive")
}

// -- Tests ----------------------------------------------------------------------

#[tokio::test]
async fn dynamic_unique_message_roundtrip() {
    let handle = spawn_calc(10).await.erase_type();

    // Tell-style dynamic send of a unique message.
    let envelope = MessageEnvelope::new(AddValue(5), None);
    handle
        .prepare_send_dynamic(envelope)
        .expect("AddValue must bind")
        .send()
        .await
        .expect("send must succeed");

    // Ask-style dynamic send of the other unique message.
    assert_eq!(dyn_ask(&handle, GetValue).await, 15);
}

#[tokio::test]
async fn dynamic_shared_message_resolves_per_actor() {
    let calc = spawn_calc(3).await.erase_type();
    let mirror = spawn_mirror().await.erase_type();

    assert_eq!(dyn_ask(&calc, Describe).await, "calc(3)");
    assert_eq!(dyn_ask(&mirror, Describe).await, "mirror");
}

#[tokio::test]
async fn unregistered_message_fails_to_bind() {
    let typed = spawn_calc(0).await;
    let erased = typed.clone().erase_type();

    let envelope = MessageEnvelope::new(Unregistered, None);
    assert!(
        erased.prepare_send_dynamic(envelope).is_none(),
        "unregistered message must not bind dynamically"
    );

    // The static dispatch path is unaffected by the missing registration.
    use factories_actor::actor::channel::ActorChannelSendable;
    typed
        .tell(Unregistered)
        .send()
        .await
        .expect("static send must succeed");
}

#[tokio::test]
async fn cross_actor_bind_misses() {
    // A message registered only for Calc must not bind on Mirror, even though
    // it has a valid dynamic dispatch ID.
    let mirror = spawn_mirror().await.erase_type();

    let envelope = MessageEnvelope::new(GetValue, None);
    assert!(mirror.prepare_send_dynamic(envelope).is_none());
}

#[test]
fn registry_assigns_distinct_ids() {
    dispatch_registry();

    let add = <AddValue as Message>::RTTI
        .dynamic_dispatch_id()
        .expect("registered message must have an ID");
    let get = <GetValue as Message>::RTTI
        .dynamic_dispatch_id()
        .expect("registered message must have an ID");
    let describe = <Describe as Message>::RTTI
        .dynamic_dispatch_id()
        .expect("registered message must have an ID");

    assert_ne!(add, get);
    assert_ne!(add, describe);
    assert_ne!(get, describe);

    // Calc's uniquely handled messages get consecutive IDs.
    assert_eq!(
        add.get().abs_diff(get.get()),
        1,
        "unique messages must be consecutive"
    );

    assert_eq!(
        <Unregistered as Message>::RTTI.dynamic_dispatch_id(),
        None,
        "unregistered messages must not receive an ID"
    );
}
